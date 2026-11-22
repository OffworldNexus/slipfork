//! `slipfork doctor`: four plain-English host rows and one verdict.
//!
//! Probing lives in the composition-root adapter ([`probe`]); this module only renders the rows,
//! their remedies, and the machine-readable object. User copy never names kernel or socket jargon.

pub mod probe;

use std::process::ExitCode;

use serde_json::json;

use crate::commands::emit;
use crate::ui::Palette;
use crate::world::HostEnv;

pub use probe::{Doctor, Part};

/// One host requirement: whether it passed, what was observed, and its remedy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Stable plain-English row name (`Storage`, `Containers`, `Daemon`, `Domains`).
    pub name: &'static str,
    /// The probe passed.
    pub ok: bool,
    /// One plain line of evidence, used verbatim by `--json`.
    pub detail: String,
    /// The exact remedy, only for a failing row.
    pub next: Option<String>,
    /// The same evidence split into styled roles for a terminal.
    pub(crate) parts: Vec<Part>,
}

impl Check {
    /// Builds a row, deriving the plain `detail` from its styled parts.
    pub(crate) fn new(
        name: &'static str,
        ok: bool,
        parts: Vec<Part>,
        next: Option<String>,
    ) -> Self {
        let detail = parts.iter().map(Part::text).collect::<Vec<_>>().join(" · ");
        Self {
            name,
            ok,
            detail,
            next,
            parts,
        }
    }
}

/// Renders and prints the doctor verdict; exit 1 when any row fails.
pub fn run(host: &HostEnv, cwd: &std::path::Path, json: bool, color: bool) -> ExitCode {
    let doctor = Doctor::probe(host, cwd);
    let ready = doctor.is_ready();
    if json {
        let checks: Vec<_> = doctor
            .checks()
            .iter()
            .map(|check| match &check.next {
                Some(next) => json!({
                    "name": check.name, "ok": check.ok, "detail": check.detail, "next": next,
                }),
                None => json!({
                    "name": check.name, "ok": check.ok, "detail": check.detail,
                }),
            })
            .collect();
        let status = emit(&format!(
            "{}\n",
            json!({ "ready": ready, "checks": checks })
        ));
        return if ready { status } else { ExitCode::FAILURE };
    }
    let palette = Palette::new(color);
    let mut out = String::new();
    if ready {
        out.push_str(&palette.tint("✓ Ready", "32;1"));
        out.push('\n');
    } else {
        let failed = doctor.checks().iter().filter(|check| !check.ok).count();
        out.push_str(&palette.tint(&format!("✗ {failed} issue(s) found"), "31;1"));
        out.push('\n');
    }
    let name_width = doctor
        .checks()
        .iter()
        .map(|check| check.name.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let separator = palette.dim("·");
    for check in doctor.checks() {
        let mark = if check.ok {
            palette.icon_ok()
        } else {
            palette.icon_bad()
        };
        let detail = check
            .parts
            .iter()
            .map(|part| part.render(&palette))
            .collect::<Vec<_>>()
            .join(&format!(" {separator} "));
        out.push_str(&format!(
            "  {mark} {:<width$}{detail}\n",
            check.name,
            width = name_width
        ));
        if let Some(next) = &check.next {
            out.push_str(&format!(
                "     {} {}\n",
                palette.kind("next:"),
                palette.command(next)
            ));
        }
    }
    let status = emit(&out);
    if ready { status } else { ExitCode::FAILURE }
}

impl Doctor {
    /// The four rows in display order.
    #[must_use]
    pub fn checks(&self) -> [&Check; 4] {
        [&self.storage, &self.containers, &self.daemon, &self.domains]
    }

    /// Every row passed.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.storage.ok && self.containers.ok && self.daemon.ok && self.domains.ok
    }
}
