//! One-way `snow.yml` → `Forkfile` conversion (`migrate_snow_yaml`).
//!
//! `Forkfile` is the only runtime manifest. [`snow`] reads the upstream Kerfufoo shape,
//! [`snow::Snow::validate`] proves every cross-reference, port, id and mode override is
//! representable, [`render`] emits the document, and this module **parses its own output** before
//! returning, so a migrated file is always statically valid. The conversion is deterministic:
//! identical input produces byte-identical output.
//!
//! The upstream top-level contract is small and fixed, and the converter deliberately ignores a key
//! it does not recognize (it cannot know the author's intent), so [`reject_unknown_top_level_keys`]
//! refuses such a section before conversion instead of silently dropping configuration.

mod access;
mod render;
mod snow;
mod template;
mod validate;

use crate::yaml::Node;

/// Every top-level key the upstream `snow.yml` schema defines.
const TOP_LEVEL_KEYS: [&str; 8] = [
    "images",
    "resources",
    "domains",
    "envs",
    "modes",
    "components",
    "ingresses",
    "hooks",
];

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
    #[error("snow.yml:{line}: unknown top-level key `{key}`\n  next: {next}")]
    UnknownKey {
        line: usize,
        key: String,
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
            Self::UnknownKey { key, .. } => format!("unknown top-level key `{key}`"),
            Self::UnsupportedExpression { expr, .. } => {
                format!("unsupported template `${{{{ {expr} }}}}`")
            }
            Self::ShellSyntax { reason, .. } => {
                format!("`run_command` uses shell syntax ({reason})")
            }
        }
    }

    /// The `next:` remedy this refusal prescribes, without the `next: ` prefix.
    pub fn remedy(&self) -> Option<&str> {
        match self {
            Self::Yaml { .. } => Some("fix the YAML indentation or scalar at that line"),
            Self::Schema { next, .. } | Self::UnknownKey { next, .. } => Some(next),
            Self::UnsupportedExpression { .. } => Some(
                "replace it with a supported `${ secrets::… | vars::… | resources::… | domains::… \
                 | components::… | deployment::… }` reference",
            ),
            Self::ShellSyntax { .. } => {
                Some("move the pipeline into scripts/<name>.sh and call it from the Forkfile")
            }
            Self::InvalidOutput { .. } => Some("fix the source manifest and re-run the migration"),
        }
    }

    /// The `file:location` this refusal points at, labelled with the manifest the caller read.
    ///
    /// `cfg/snow.yml:12` for a YAML or unknown-key refusal, `cfg/snow.yml:components[0].run_command`
    /// for a schema refusal, and `Forkfile:7:3` when the emitted document fails to re-parse.
    pub fn site(&self, manifest: &str) -> String {
        match self {
            Self::Yaml { line, .. } | Self::UnknownKey { line, .. } => {
                format!("{manifest}:{line}")
            }
            Self::Schema { path, .. }
            | Self::UnsupportedExpression { path, .. }
            | Self::ShellSyntax { path, .. } => format!("{manifest}:{path}"),
            Self::InvalidOutput { line, col, .. } => format!("Forkfile:{line}:{col}"),
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

/// Refuse the first top-level section the upstream schema does not define.
///
/// Only root map keys are inspected: nested keys are either consumed by the reader or proven
/// representable by its own schema pass, while a stray top-level section is the one silent-drop
/// hazard the converter cannot diagnose on its own.
fn reject_unknown_top_level_keys(root: &Node) -> Result<(), MigrateError> {
    let Some(entries) = root.as_map() else {
        return Ok(());
    };
    for entry in entries {
        if TOP_LEVEL_KEYS.contains(&entry.key.as_str()) {
            continue;
        }
        return Err(MigrateError::UnknownKey {
            line: entry.line,
            key: entry.key.clone(),
            next: format!(
                "rename `{}` to a supported key ({})",
                entry.key,
                TOP_LEVEL_KEYS.join(", ")
            ),
        });
    }
    Ok(())
}

/// Convert one upstream `snow.yml` document into an idiomatic, statically valid `Forkfile`.
pub fn migrate_snow_yaml(source: &str) -> Result<String, MigrateError> {
    let root = crate::yaml::parse(source)?;
    reject_unknown_top_level_keys(&root)?;
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

    #[test]
    fn unknown_top_level_keys_are_refused_with_a_line_and_a_schema_remedy() {
        let root =
            crate::yaml::parse("images: []\nservices:\n  - id: api\n").expect("parse the document");
        let error =
            reject_unknown_top_level_keys(&root).expect_err("an unknown section is refused");
        assert_eq!(error.message(), "unknown top-level key `services`");
        assert_eq!(error.site("cfg/snow.yml"), "cfg/snow.yml:2");
        assert!(
            error.remedy().unwrap_or_default().contains("components"),
            "the remedy lists the schema: {error}"
        );
    }

    #[test]
    fn only_root_keys_are_inspected_so_nested_and_commented_lines_pass() {
        let root = crate::yaml::parse(
            "# upstream\n---\nimages: []\nmodes:\n  all:\n    envs:\n      app:\n        services: nested\n",
        )
        .expect("parse the document");
        reject_unknown_top_level_keys(&root).expect("every root key is defined");
        let documented =
            crate::yaml::parse("images: []\ncomponents:\n  - id: api\n    image: app\n")
                .expect("parse a supported document");
        reject_unknown_top_level_keys(&documented).expect("a supported document passes");
    }

    #[test]
    fn a_migrated_document_never_carries_an_unknown_root_section() {
        let error = migrate_snow_yaml("images: []\nservices: []\n")
            .expect_err("the converter refuses before rendering");
        assert!(matches!(error, MigrateError::UnknownKey { line: 2, .. }));
    }
}
