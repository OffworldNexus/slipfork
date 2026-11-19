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

impl MigrateError {
    /// The one human sentence this refusal carries, without the `snow.yml:<path>:` prefix.
    ///
    /// A daemon prefixes it with the manifest path it read, so a refused boot stays one line.
    pub fn message(&self) -> String {
        match self {
            Self::Yaml { message, .. }
            | Self::Schema { message, .. }
            | Self::InvalidOutput { message, .. } => message.clone(),
            Self::UnsupportedExpression { expr, .. } => {
                format!("unsupported template `${{{{ {expr} }}}}`")
            }
            Self::ShellSyntax { reason, .. } => {
                format!("`run_command` uses shell syntax ({reason})")
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::FeatureSet;

    #[test]
    fn mode_envs_become_feature_gated_groups() {
        let source = r#"
images:
  - id: app
    context: .
components:
  - id: api
    image: app
    run_command: "exec sleep 300"
modes:
  local:
    envs:
      app:
        DEBUG: "${{ vars::DEBUG }}"
"#;
        let output = migrate_snow_yaml(source).expect("migrate");
        assert!(
            output.contains("features {\n    local\n}"),
            "the mode becomes a feature: {output}"
        );
        assert!(
            output.contains("on local {\n    env app {\n        DEBUG = var(DEBUG)\n    }\n}"),
            "the mode env group is feature-gated: {output}"
        );
        let active = crate::parse_forkfile(&output)
            .expect("parse")
            .evaluate(&FeatureSet::with_features(["local"]))
            .expect("evaluate with the active mode");
        assert_eq!(active.env_groups.len(), 1);
        assert_eq!(active.env_groups[0].name, "app");
        let inactive = crate::parse_forkfile(&output)
            .expect("parse")
            .evaluate(&FeatureSet::new())
            .expect("evaluate without the mode");
        assert!(
            inactive.env_groups.is_empty(),
            "the mode group is inactive without its feature"
        );
    }
}
