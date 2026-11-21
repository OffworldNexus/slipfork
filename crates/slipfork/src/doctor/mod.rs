//! `slipfork doctor`: exactly three host requirements, one verdict.
//!
//! Probing lives in the composition-root adapter ([`probe`]); this module only renders the three
//! rows, their remedies, and the machine-readable object.

pub mod probe;

use std::process::ExitCode;

use serde_json::json;

use crate::commands::emit;
use crate::render::paint;
use crate::world::HostEnv;

pub use probe::Doctor;

/// One host requirement: whether it passed, what was observed, and its remedy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Stable row name (`Btrfs`, `Namespaces`, `Runtime`).
    pub name: &'static str,
    /// The probe passed.
    pub ok: bool,
    /// One line of evidence.
    pub detail: String,
    /// The exact remedy, only for a failing row.
    pub next: Option<String>,
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
    let mut out = String::new();
    if ready {
        out.push_str(&paint("✓ Ready", "32", color));
        out.push('\n');
    } else {
        let failed = doctor.checks().iter().filter(|check| !check.ok).count();
        out.push_str(&paint(&format!("✗ {failed} issue(s)"), "31", color));
        out.push('\n');
    }
    let name_width = doctor
        .checks()
        .iter()
        .map(|check| check.name.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    for check in doctor.checks() {
        let mark = if check.ok {
            paint("✓", "32", color)
        } else {
            paint("✗", "31", color)
        };
        out.push_str(&format!(
            "  {mark} {:<width$}{}\n",
            check.name,
            check.detail,
            width = name_width
        ));
        if let Some(next) = &check.next {
            out.push_str(&format!("     next: {next}\n"));
        }
    }
    let status = emit(&out);
    if ready { status } else { ExitCode::FAILURE }
}

impl Doctor {
    /// The three rows in display order.
    #[must_use]
    pub fn checks(&self) -> [&Check; 3] {
        [&self.btrfs, &self.namespaces, &self.runtime]
    }

    /// Every row passed.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.btrfs.ok && self.namespaces.ok && self.runtime.ok
    }
}
