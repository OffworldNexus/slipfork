//! `slipfork open`: the world root URL, or one service route, headless-aware.

use std::process::{Command, ExitCode, Stdio};

use serde_json::json;

use crate::commands::{Session, emit};
use crate::ui::Palette;

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
        let world = crate::world::world_name(&self.cwd, None)
            .map(|world| world.as_str().to_owned())
            .unwrap_or_else(|_| "slipfork".to_owned());
        let base = format!("http://{world}.localhost");
        // No argument selects the world root (`/`), so a multi-route Forkfile never refuses `open`.
        let candidates = match route_candidates(&self.cwd, &base, args.service.as_deref()) {
            Ok(candidates) => candidates,
            Err(message) => {
                return self.report(
                    "forkfile",
                    &message,
                    "create a Forkfile in this directory, then run `slipfork up`",
                    args.json,
                );
            }
        };
        let url = if args.service.is_some() {
            if candidates.len() > 1 {
                if !args.json {
                    let mut text = String::new();
                    for candidate in &candidates {
                        text.push_str(&format!("{}\n", Palette::new(self.color()).url(candidate)));
                    }
                    let _ = emit(&text);
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
            return emit(&format!(
                "{}\n",
                json!({ "url": url, "opened": !headless && self.host.opener.is_some() })
            ));
        }
        if headless {
            return emit(&format!(
                "{} {}\nnext: open it on a machine with a browser\n",
                Palette::new(self.color()).icon_info(),
                Palette::new(self.color()).url(&url)
            ));
        }
        let Some(opener) = &self.host.opener else {
            return emit(&format!(
                "{}\nnext: install xdg-open, or open the URL in a browser\n",
                Palette::new(self.color()).url(&url)
            ));
        };
        match Command::new(opener).arg(&url).stdin(Stdio::null()).status() {
            Ok(status) if status.success() => emit(&format!(
                "{} Opened {}\n",
                Palette::new(self.color()).icon_ok(),
                Palette::new(self.color()).url(&url)
            )),
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
) -> Result<Vec<String>, String> {
    let path = cwd.join("Forkfile");
    let source = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
    let checked = slipfork_forkfile::compile(&source).map_err(|diagnostics| {
        diagnostics.first().map_or_else(
            || "invalid Forkfile".to_owned(),
            |item| format!("Forkfile:{}: {}", item.span.line(), item.message),
        )
    })?;
    let declared = checked.document.services();
    if let Some(name) = service
        && !declared.iter().any(|declared| declared.id == name)
    {
        return Err(format!("`{name}` is not a service in this Forkfile"));
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
