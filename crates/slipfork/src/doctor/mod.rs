//! `slipfork doctor`: four plain-English host rows and one verdict.
//!
//! Probing lives in the composition-root adapter ([`probe`]); this module only renders the rows,
//! their remedies, and the machine-readable object. User copy never names kernel or socket jargon.

pub mod probe;

use std::process::ExitCode;

use serde_json::json;

use crate::ui::report::Palette;
use crate::ui::report::ReportRenderer;
use crate::ui::report::{Column, ColumnWidths, Table};
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
pub fn run(
    host: &HostEnv,
    cwd: &std::path::Path,
    json: bool,
    renderer: &ReportRenderer,
) -> ExitCode {
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
        let status = renderer.json_exit(&json!({ "ready": ready, "checks": checks }));
        return if ready { status } else { ExitCode::FAILURE };
    }
    let palette = Palette::new(renderer.color());
    let mut lines = Vec::new();
    if ready {
        lines.push(palette.tint("✓ Ready", "32;1"));
    } else {
        let failed = doctor.checks().iter().filter(|check| !check.ok).count();
        lines.push(palette.tint(&format!("✗ {failed} issue(s) found"), "31;1"));
    }
    let separator = palette.meta("·");
    let widths = widths(&doctor);
    let table = Table::new(&CHECK).indent("  ");
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
        lines.push(table.line(&widths, &[mark, check.name.to_owned(), detail]));
        if let Some(next) = &check.next {
            lines.push(format!(
                "     {} {}",
                palette.kind("next:"),
                palette.command(next)
            ));
        }
    }
    let status = renderer.emit_lines_exit(lines);
    if ready { status } else { ExitCode::FAILURE }
}

/// The three doctor columns: mark, plain-English row name, evidence.
const CHECK: [Column; 3] = [Column::left().at_least(1), Column::left(), Column::left()];

/// The one borderless grid shared by all four rows.
fn widths(doctor: &Doctor) -> ColumnWidths {
    let rows: Vec<Vec<String>> = doctor
        .checks()
        .iter()
        .map(|check| vec![String::new(), check.name.to_owned(), check.detail.clone()])
        .collect();
    Table::new(&CHECK).widths(&rows)
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

#[cfg(test)]
mod tests {
    use super::{CHECK, Check, Doctor, Part, widths};
    use crate::ui::report::Palette;
    use crate::ui::report::Table;

    fn doctor() -> Doctor {
        Doctor {
            storage: Check::new(
                "Storage",
                true,
                vec![
                    Part::Bold("Btrfs".to_owned()),
                    Part::Meta("on ~/demo".to_owned()),
                ],
                None,
            ),
            containers: Check::new(
                "Containers",
                false,
                vec![
                    Part::Bold("crun".to_owned()),
                    Part::Meta("not installed".to_owned()),
                ],
                Some("install crun".to_owned()),
            ),
            daemon: Check::new(
                "Daemon",
                true,
                vec![Part::Meta("socket present".to_owned())],
                None,
            ),
            domains: Check::new(
                "Domains",
                true,
                vec![Part::Url("http://*.localhost".to_owned())],
                None,
            ),
        }
    }

    #[test]
    fn every_check_row_starts_its_evidence_on_one_column() {
        let doctor = doctor();
        let palette = Palette::new(false);
        let widths = widths(&doctor);
        let table = Table::new(&CHECK).indent("  ");
        let mut evidence_at = None;
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
                .join(" · ");
            let row = table.line(&widths, &[mark, check.name.to_owned(), detail.clone()]);
            assert!(
                !row.contains('\u{1b}'),
                "a plain doctor row stays pure: {row:?}"
            );
            assert!(row.starts_with("  "), "{row:?}");
            let at = row.rfind(detail.as_str()).expect("evidence is present");
            assert_eq!(*evidence_at.get_or_insert(at), at, "one evidence column");
            assert!(row.contains(check.name), "{row:?}");
        }
    }
}
