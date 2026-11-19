//! One-way `snow.yml` → `Forkfile` conversion (`migrate_snow_yaml`).
//!
//! `Forkfile` is the only runtime manifest. [`snow`] reads the upstream Kerfufoo shape,
//! [`snow::Snow::validate`] proves every cross-reference, port, id and mode override is
//! representable, [`render`] emits the document, and this module **parses its own output** before
//! returning, so a migrated file is always statically valid. The conversion is deterministic:
//! identical input produces byte-identical output.

mod render;
mod snow;
mod template;
mod validate;

/// A `snow.yml` document that cannot be migrated faithfully.
#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("snow.yml:{line}: {message}\n  next: fix the YAML indentation or scalar at that line")]
    Yaml { line: usize, message: String },
    #[error("snow.yml:{path}: {message}\n  next: {next}")]
    Schema {
        path: String,
        message: String,
        next: String,
    },
    #[error(
        "snow.yml:{path}: unsupported template `${{{{ {expr} }}}}`\n  next: replace it with a \
         supported `${{{{ secrets::… | vars::… | resources::… | domains::… | components::… | \
         deployment::… }}}}` reference"
    )]
    UnsupportedExpression { path: String, expr: String },
    #[error(
        "snow.yml:{path}: `run_command` uses shell syntax ({reason}); Forkfile runs direct argv \
         with no shell\n  next: move the pipeline into scripts/<name>.sh and call it from the \
         Forkfile"
    )]
    ShellSyntax { path: String, reason: String },
    #[error(
        "migrated Forkfile is invalid at Forkfile:{line}:{col}: {message}\n  next: fix the source \
         manifest and re-run the migration"
    )]
    InvalidOutput {
        line: u32,
        col: u32,
        message: String,
    },
}

impl From<crate::yaml::YamlError> for MigrateError {
    fn from(error: crate::yaml::YamlError) -> Self {
        Self::Yaml {
            line: error.line,
            message: error.message,
        }
    }
}

pub(super) fn schema(
    path: impl Into<String>,
    message: impl Into<String>,
    next: impl Into<String>,
) -> MigrateError {
    MigrateError::Schema {
        path: path.into(),
        message: message.into(),
        next: next.into(),
    }
}

/// Convert one upstream `snow.yml` document into an idiomatic, statically valid `Forkfile`.
pub fn migrate_snow_yaml(source: &str) -> Result<String, MigrateError> {
    let root = crate::yaml::parse(source)?;
    let snow = snow::Snow::read(&root)?;
    snow.validate()?;
    let output = render::render(&snow);
    match crate::parse_forkfile(&output) {
        Ok(_) => Ok(output),
        Err(diagnostics) => {
            let diagnostic = &diagnostics[0];
            Err(MigrateError::InvalidOutput {
                line: diagnostic.span.line,
                col: diagnostic.span.col,
                message: diagnostic.message.clone(),
            })
        }
    }
}
