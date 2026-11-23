//! `fmt` and `migrate`: `Forkfile` formatting and legacy-manifest conversion.
//!
//! Both verbs share one session-injected working directory and one failure renderer; the module
//! never re-probes the environment or the terminal.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::json;
use slipfork_forkfile::Diagnostics;

use crate::ui::{Palette, paint};

use super::{Session, emit};

/// Parsed `slipfork fmt` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FmtArgs {
    /// Report only; never rewrite the file.
    pub check: bool,
    /// Emit the machine-readable object instead of human text.
    pub json: bool,
    /// The `Forkfile` path, relative to the working directory.
    pub path: PathBuf,
}

/// Parsed `slipfork migrate` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateArgs {
    /// Save the result as `Forkfile` next to the input.
    pub write: bool,
    /// Overwrite an existing `Forkfile`.
    pub force: bool,
    /// Emit the machine-readable object instead of the `Forkfile` text.
    pub json: bool,
    /// The legacy manifest path, relative to the working directory.
    pub path: PathBuf,
}

impl Session {
    /// Formats a `Forkfile` in place, or reports whether it needs formatting.
    pub fn fmt(&self, args: &FmtArgs) -> ExitCode {
        let path = self.cwd.join(&args.path);
        let label = args.path.display().to_string();
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                let message = format!("cannot read {label}: {error}");
                let next = format!("create {label} or pass the path of an existing Forkfile");
                return self.report_details(
                    "io",
                    &message,
                    Some(json!({ "path": label })),
                    &next,
                    args.json,
                );
            }
        };
        let checked = match slipfork_forkfile::compile(&source) {
            Ok(checked) => checked,
            Err(diagnostics) => {
                return self.fmt_failure(&label, &diagnostics, &source, args.json);
            }
        };
        let formatted = slipfork_forkfile::format(&checked.document);
        let canonical = formatted == source;
        let changed = !canonical;
        if !args.check
            && changed
            && let Err(error) = fs::write(&path, &formatted)
        {
            let message = format!("cannot write {label}: {error}");
            let next = format!("check that {} is writable", path.display());
            return self.report_details(
                "io",
                &message,
                Some(json!({ "path": label })),
                &next,
                args.json,
            );
        }
        if args.json {
            let status = emit(&format!(
                "{}\n",
                json!({ "path": label, "formatted": canonical, "changed": changed })
            ));
            return if args.check && changed {
                ExitCode::FAILURE
            } else {
                status
            };
        }
        if args.check {
            if canonical {
                return emit(&format!(
                    "{} {label} is formatted\n",
                    Palette::new(self.color()).icon_ok()
                ));
            }
            let diff = slipfork_forkfile::fmt::render_diff(&source, &formatted, &label);
            eprint!("{}", tint_diff(&diff, self.color()));
            eprintln!("next: slipfork fmt {label}");
            return ExitCode::FAILURE;
        }
        if changed {
            emit(&format!(
                "{} formatted {label}\n",
                Palette::new(self.color()).icon_ok()
            ))
        } else {
            emit(&format!(
                "{} {label} is already formatted\n",
                Palette::new(self.color()).icon_ok()
            ))
        }
    }

    /// Renders the compiler's first diagnostic for `fmt`, as a caret block or one JSON object.
    fn fmt_failure(
        &self,
        label: &str,
        diagnostics: &Diagnostics,
        source: &str,
        json: bool,
    ) -> ExitCode {
        if json {
            let first = diagnostics.first();
            return self.report_details(
                "forkfile",
                first.map_or("invalid Forkfile", |item| item.message.as_str()),
                Some(json!({
                    "path": label,
                    "line": first.map(|item| item.span.line()),
                    "col": first.map(|item| item.span.col()),
                })),
                first.map_or("fix the Forkfile", |item| item.next.as_str()),
                true,
            );
        }
        eprintln!("{}", diagnostics.render_as(source, label));
        ExitCode::FAILURE
    }

    /// Converts a legacy `snow.yml` manifest into a canonical `Forkfile`.
    pub fn migrate(&self, args: &MigrateArgs) -> ExitCode {
        let path = self.cwd.join(&args.path);
        let label = args.path.display().to_string();
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                let message = format!("cannot read {label}: {error}");
                let next = format!("create {label} or pass the path of an existing snow.yml");
                return self.report_details(
                    "io",
                    &message,
                    Some(json!({ "path": label })),
                    &next,
                    args.json,
                );
            }
        };
        let forkfile = match slipfork_forkfile::migrate(&source) {
            Ok(forkfile) => forkfile,
            Err(diags) => {
                if args.json {
                    let first = diags.first();
                    return self.report_details(
                        "forkfile",
                        first.map_or("invalid snow.yml", |item| item.message.as_str()),
                        Some(json!({
                            "path": label,
                            "line": first.map(|item| item.span.line()),
                            "col": first.map(|item| item.span.col()),
                        })),
                        first.map_or("fix the manifest", |item| item.next.as_str()),
                        true,
                    );
                }
                eprintln!("{}", diags.render_as(&source, &label));
                return ExitCode::FAILURE;
            }
        };
        let output = path.with_file_name("Forkfile");
        let existed = output.exists();
        if args.write && existed && !args.force {
            let message = "refusing to overwrite an existing Forkfile".to_string();
            let next = format!("mv Forkfile {}", "Forkfile.old");
            return self.report_details(
                "usage",
                &message,
                Some(json!({ "path": label })),
                &next,
                args.json,
            );
        }
        if args.write {
            if let Err(error) = fs::write(&output, &forkfile) {
                let message = format!("cannot write {}: {error}", output.display());
                let next = format!("check that {} is writable", output.display());
                return self.report_details(
                    "io",
                    &message,
                    Some(json!({ "path": label })),
                    &next,
                    args.json,
                );
            }
            if existed {
                eprintln!("overwriting Forkfile (--force)");
            }
        }
        if args.json {
            let status = emit(&format!(
                "{}\n",
                json!({ "path": label, "written": args.write, "forkfile": forkfile })
            ));
            if args.write {
                eprintln!("next: slipfork fmt --check Forkfile");
            }
            return status;
        }
        let status = emit(&forkfile);
        if args.write {
            eprintln!("next: slipfork fmt --check Forkfile");
        }
        status
    }
}

/// Tints diff removals red and additions green; never emits an escape otherwise.
fn tint_diff(diff: &str, color: bool) -> String {
    let mut out = String::new();
    for line in diff.lines() {
        let code = if line.starts_with("---") || line.starts_with("+++") || line.starts_with("@@") {
            Some("1")
        } else if line.starts_with('+') {
            Some("32")
        } else if line.starts_with('-') {
            Some("31")
        } else {
            None
        };
        match (color, code) {
            (true, Some(code)) => out.push_str(&paint(line, code, true)),
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    out
}
