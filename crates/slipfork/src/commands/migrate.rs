//! `slipfork migrate`: one-way upstream `snow.yml` → local `Forkfile` conversion.
//!
//! The converter ([`slipfork_forkfile::migrate_snow_yaml`]) owns every YAML, template, argv, and
//! unknown-section rule and re-parses its own output, so a converted file is statically valid by
//! construction. This module only resolves the file pair, writes the bytes, and renders the one
//! summary line (`snow.yml` → `Forkfile`).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::json;
use slipfork_forkfile::{ForkFile, MigrateError, Stmt, migrate_snow_yaml, parse_forkfile};

use crate::CliApp;

/// One actionable `error:` line, its `next:` remedy, and the `--json` `code`.
#[derive(Debug)]
struct Failure {
    code: &'static str,
    error: String,
    next: String,
}

impl Failure {
    fn new(code: &'static str, error: impl Into<String>, next: impl Into<String>) -> Self {
        Self {
            code,
            error: error.into(),
            next: next.into(),
        }
    }

    /// Print the refusal in the §3 shape, then hand back the failing exit code.
    fn report(&self, app: &CliApp) -> ExitCode {
        if app.output_mode.json {
            println!(
                "{}",
                json!({
                    "ok": false,
                    "code": self.code,
                    "error": self.error,
                    "next": self.next,
                })
            );
        } else {
            eprintln!("error: {}\n  next: {}", self.error, self.next);
        }
        ExitCode::from(1)
    }
}

/// One resolved upstream manifest: the absolute path to read and the label the summary prints.
struct InputFile {
    path: PathBuf,
    label: String,
}

/// Where the converted document goes.
#[derive(Debug)]
enum Destination {
    /// `--stdout`: print the conversion and write nothing.
    Stdout,
    /// Write `path`, replacing an existing file only under `force`.
    File { path: PathBuf, force: bool },
}

/// One in-flight conversion: the resolved input, the chosen destination, and its invoking CLI.
struct Migration<'a> {
    app: &'a CliApp,
    cwd: PathBuf,
    input: InputFile,
    destination: Destination,
}

impl<'a> Migration<'a> {
    /// Resolve the input manifest and the destination from the parsed flags.
    fn new(
        app: &'a CliApp,
        file: Option<PathBuf>,
        output: Option<PathBuf>,
        stdout: bool,
        force: bool,
    ) -> Result<Self, Failure> {
        let cwd = app.ui.cwd().map_err(|err| {
            Failure::new(
                "no_cwd",
                err,
                "run `slipfork migrate` from the directory that holds the manifest",
            )
        })?;
        let input = resolve_input(&cwd, file.as_deref())?;
        let destination = if stdout {
            Destination::Stdout
        } else {
            Destination::File {
                path: output.unwrap_or_else(|| PathBuf::from("Forkfile")),
                force,
            }
        };
        Ok(Self {
            app,
            cwd,
            input,
            destination,
        })
    }

    /// Convert the manifest and deliver it to the chosen destination.
    fn run(&self) -> Result<(), Failure> {
        let source = fs::read_to_string(&self.input.path).map_err(|err| {
            Failure::new(
                "missing_input",
                format!("cannot read {}: {err}", self.input.label),
                "check the path and the file permissions, then re-run `slipfork migrate`",
            )
        })?;
        let converted = migrate_snow_yaml(&source)
            .map_err(|error| conversion_failure(&self.input.label, error))?;
        let summary = Identifiers::of(&converted)?;
        match &self.destination {
            Destination::Stdout => {
                if self.app.output_mode.json {
                    let document = serde_json::to_string(&converted).map_err(|err| {
                        Failure::new(
                            "write_failed",
                            format!("cannot render the JSON document: {err}"),
                            "re-run `slipfork migrate --stdout` without `--json`",
                        )
                    })?;
                    println!("{document}");
                } else {
                    print!("{converted}");
                }
                Ok(())
            }
            Destination::File { path, force } => {
                let target_path = absolute(&self.cwd, path);
                let target_label = path.display().to_string();
                if !*force && target_path.exists() {
                    return Err(Failure::new(
                        "output_exists",
                        format!("refusing to overwrite {target_label}"),
                        "pass -f/--force to replace it, or --stdout to preview the conversion",
                    ));
                }
                fs::write(&target_path, &converted).map_err(|err| {
                    Failure::new(
                        "write_failed",
                        format!("cannot write {target_label}: {err}"),
                        "check the parent directory exists and is writable, then re-run \
                         `slipfork migrate`",
                    )
                })?;
                self.report_success(&target_label, &summary);
                Ok(())
            }
        }
    }

    /// Render the `✓ Migrated …` line, or the equivalent JSON document.
    fn report_success(&self, target: &str, summary: &Identifiers) {
        if self.app.output_mode.json {
            println!(
                "{}",
                json!({
                    "ok": true,
                    "input": self.input.label,
                    "output": target,
                    "services": summary.services.len(),
                    "pg_clusters": summary.pg_clusters.len(),
                    "env_groups": summary.env_groups.len(),
                })
            );
        } else {
            println!(
                "{}",
                success_line(self.app, &self.input.label, target, summary)
            );
        }
    }
}

/// Convert `snow.yml` into a `Forkfile`, or preview the conversion without writing.
pub(crate) fn run(
    app: &CliApp,
    file: Option<PathBuf>,
    output: Option<PathBuf>,
    stdout: bool,
    force: bool,
) -> Result<ExitCode, String> {
    let migration = match Migration::new(app, file, output, stdout, force) {
        Ok(migration) => migration,
        Err(failure) => return Ok(failure.report(app)),
    };
    match migration.run() {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(failure) => Ok(failure.report(app)),
    }
}

/// Resolve the input manifest: the explicit `FILE`, or `./snow.yml` then `./snow.yaml`.
fn resolve_input(cwd: &Path, requested: Option<&Path>) -> Result<InputFile, Failure> {
    if let Some(requested) = requested {
        let path = absolute(cwd, requested);
        if !path.is_file() {
            return Err(Failure::new(
                "missing_input",
                format!("no input file at {}", requested.display()),
                "create it, or run `slipfork migrate <FILE>` with the existing snow.yml",
            ));
        }
        return Ok(InputFile {
            path,
            label: requested.display().to_string(),
        });
    }
    for name in ["snow.yml", "snow.yaml"] {
        let path = cwd.join(name);
        if path.is_file() {
            return Ok(InputFile {
                path,
                label: name.to_owned(),
            });
        }
    }
    Err(Failure::new(
        "missing_input",
        "no snow.yml or snow.yaml in the current directory",
        "add the upstream manifest, or run `slipfork migrate <FILE>`",
    ))
}

/// Resolve a possibly-relative path against the invoking directory.
fn absolute(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Project one converter refusal onto the `error:` line, the `next:` remedy, and a stable code.
fn conversion_failure(manifest: &str, error: MigrateError) -> Failure {
    let code = match &error {
        MigrateError::UnknownKey { .. } => "unknown_key",
        MigrateError::Yaml { .. }
        | MigrateError::Schema { .. }
        | MigrateError::UnsupportedExpression { .. }
        | MigrateError::ShellSyntax { .. }
        | MigrateError::InvalidOutput { .. } => "invalid_manifest",
    };
    Failure::new(
        code,
        format!("{}: {}", error.site(manifest), error.message()),
        error
            .remedy()
            .unwrap_or("fix the upstream manifest, then re-run `slipfork migrate`"),
    )
}

/// The distinct entities the converted document declares, keyed by id.
///
/// A service declared once and re-declared inside `on <feature>` is one service, so every count is
/// a set length rather than a walk tally.
#[derive(Debug, Default)]
struct Identifiers {
    services: BTreeSet<String>,
    pg_clusters: BTreeSet<String>,
    redis: BTreeSet<String>,
    s3: BTreeSet<String>,
    env_groups: BTreeSet<String>,
}

impl Identifiers {
    /// Parse the converted document and collect every declared entity (feature-gated ones included).
    fn of(converted: &str) -> Result<Self, Failure> {
        let forkfile: ForkFile = parse_forkfile(converted).map_err(|diagnostics| {
            Failure::new(
                "invalid_manifest",
                format!(
                    "converted Forkfile does not parse: {}",
                    diagnostics
                        .first()
                        .map(|diagnostic| diagnostic.message.as_str())
                        .unwrap_or("unknown syntax error")
                ),
                "fix the upstream manifest, then re-run `slipfork migrate`",
            )
        })?;
        let mut identifiers = Self::default();
        Self::collect(&forkfile.statements, &mut identifiers);
        Ok(identifiers)
    }

    fn collect(statements: &[Stmt], identifiers: &mut Self) {
        for statement in statements {
            match statement {
                Stmt::Service(service) => {
                    identifiers.services.insert(service.id.value.clone());
                }
                Stmt::Postgres(cluster) => {
                    identifiers.pg_clusters.insert(cluster.id.value.clone());
                }
                Stmt::Redis(cluster) => {
                    identifiers.redis.insert(cluster.id.value.clone());
                }
                Stmt::S3(bucket) => {
                    identifiers.s3.insert(bucket.id.value.clone());
                }
                Stmt::EnvGroup(group) => {
                    identifiers.env_groups.insert(group.name.value.clone());
                }
                Stmt::On(block) => Self::collect(&block.body, identifiers),
                Stmt::Project(_) | Stmt::Features { .. } | Stmt::Domain(_) | Stmt::Task(_) => {}
            }
        }
    }

    /// `2 services, 1 postgres cluster, 3 env groups`; an empty resource kind is not mentioned.
    fn describe(&self) -> String {
        let mut parts = vec![counted(self.services.len(), "service", "services")];
        if !self.pg_clusters.is_empty() {
            parts.push(counted(
                self.pg_clusters.len(),
                "postgres cluster",
                "postgres clusters",
            ));
        }
        if !self.redis.is_empty() {
            parts.push(counted(self.redis.len(), "redis cluster", "redis clusters"));
        }
        if !self.s3.is_empty() {
            parts.push(counted(self.s3.len(), "s3 bucket", "s3 buckets"));
        }
        if !self.env_groups.is_empty() {
            parts.push(counted(self.env_groups.len(), "env group", "env groups"));
        }
        parts.join(", ")
    }
}

/// `1 service` / `2 services`.
fn counted(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {plural}")
    }
}

/// `✓ Migrated snow.yml → Forkfile (2 services, 1 postgres cluster, 3 env groups)`.
fn success_line(app: &CliApp, input: &str, output: &str, summary: &Identifiers) -> String {
    let ui = &app.terminal;
    format!(
        "{} Migrated {} → {} ({})",
        ui.ok("✓"),
        ui.bold(input),
        ui.bold(output),
        ui.secondary(&summary.describe()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_pluralizes_and_omits_empty_resource_kinds() {
        let mut identifiers = Identifiers::default();
        identifiers.services.insert("api".to_owned());
        identifiers.env_groups.insert("common".to_owned());
        assert_eq!(identifiers.describe(), "1 service, 1 env group");

        identifiers.services.insert("front".to_owned());
        identifiers.pg_clusters.insert("pg".to_owned());
        identifiers.redis.insert("cache".to_owned());
        identifiers.s3.insert("uploads".to_owned());
        assert_eq!(
            identifiers.describe(),
            "2 services, 1 postgres cluster, 1 redis cluster, 1 s3 bucket, 1 env group"
        );
    }

    #[test]
    fn feature_gated_services_are_counted_once_by_id() {
        let identifiers = Identifiers::of(
            r#"features {
    cloud
}
on cloud {
    service api ./api {
        run exec sleep 300
    }
}
"#,
        )
        .expect("a feature-gated service is a valid Forkfile");
        assert_eq!(identifiers.services.len(), 1);
        assert_eq!(identifiers.describe(), "1 service");
    }
}
