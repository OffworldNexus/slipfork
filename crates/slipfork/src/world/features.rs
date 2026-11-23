//! Per-world feature configuration persisted inside the project snapshot.
//!
//! `<project>/.slipfork/features.json` is the single source of truth for the feature edits a world
//! boots with. `slipfork fork` inherits it for free because the file lives inside the snapshotted
//! checkout; `up`/`fork`/`features` write it, while `env` only ever evaluates an override.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use slipfork_forkfile::ast::Forkfile;
use slipfork_forkfile::{Diagnostic, FeatureEdit, FeatureSet};

/// The persisted file, relative to the project root.
pub const FEATURES_FILE: &str = ".slipfork/features.json";

/// The file-format version this binary writes and accepts.
const VERSION: u32 = 1;

const fn version() -> u32 {
    VERSION
}

/// The feature configuration one world boots with: an optional empty base plus ordered edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldFeatures {
    /// File-format version; a missing key defaults to the current version.
    #[serde(default = "version")]
    pub version: u32,
    /// Start from an empty base instead of built-in `dev` plus `features { default }`.
    #[serde(default)]
    pub no_default: bool,
    /// Ordered edits applied on top of the base (`+mail`, `-dev`, `!mail`).
    #[serde(default)]
    pub edits: Vec<FeatureEdit>,
}

impl Default for WorldFeatures {
    fn default() -> Self {
        Self {
            version: VERSION,
            no_default: false,
            edits: Vec::new(),
        }
    }
}

impl WorldFeatures {
    /// A configuration from one invocation's `-F`/`--no-default-features` flags.
    #[must_use]
    pub fn new(no_default: bool, edits: Vec<FeatureEdit>) -> Self {
        Self {
            version: VERSION,
            no_default,
            edits,
        }
    }

    /// `<project>/.slipfork/features.json`.
    #[must_use]
    pub fn path(project: &Path) -> PathBuf {
        project.join(FEATURES_FILE)
    }

    /// The one actionable remedy for an unreadable or malformed file.
    pub const REMEDY: &str = "fix or delete `.slipfork/features.json`";

    /// Reads the world's file: an absent file is the default, a malformed one is an error.
    ///
    /// # Errors
    ///
    /// Returns one line naming the path when the file exists but cannot be read or parsed.
    pub fn load(project: &Path) -> Result<Self, String> {
        let path = Self::path(project);
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| {
                format!("`{}` is not valid features JSON: {error}", path.display())
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("cannot read `{}`: {error}", path.display())),
        }
    }

    /// Rewrites the file atomically (`tmp` + rename), so a reader never sees a half-written edit list.
    ///
    /// # Errors
    ///
    /// Propagates directory creation, serialization, write, and rename failures.
    pub fn save(&self, project: &Path) -> Result<(), String> {
        let path = Self::path(project);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create `{}`: {error}", parent.display()))?;
        }
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        fs::write(&temporary, bytes)
            .map_err(|error| format!("write `{}`: {error}", temporary.display()))?;
        fs::rename(&temporary, &path)
            .map_err(|error| format!("rename `{}`: {error}", path.display()))
    }

    /// Resolves this configuration against `doc` through the compiler's canonical expansion.
    ///
    /// # Errors
    ///
    /// Returns the compiler diagnostic naming every unknown feature and its remedy.
    pub fn resolve(&self, doc: &Forkfile) -> Result<FeatureSet, Diagnostic> {
        doc.resolve_features(self.no_default, &self.edits)
    }

    /// Appends `edits` in order, so a later edit always wins over an earlier one.
    pub fn apply(&mut self, edits: &[FeatureEdit]) {
        self.edits.extend_from_slice(edits);
    }

    /// The configuration one invocation boots with: `request` merged over the persisted file.
    ///
    /// An absent file is the default base; `request.no_default` is an explicit reset of the base,
    /// otherwise the persisted base and edits are kept; `request.edits` are appended last. Edits
    /// compact last-wins, so a later `+mail`/`-mail` replaces the earlier edit for that name.
    /// Saving is the caller's job: `env` never writes, `up`/`fork` persist the result.
    ///
    /// # Errors
    ///
    /// Returns the one-line remedy when the persisted file exists but cannot be read or parsed.
    pub fn merged(project: &Path, request: &Self) -> Result<Self, String> {
        let mut config = if request.no_default {
            Self::new(true, Vec::new())
        } else {
            Self::load(project)?
        };
        config.apply(&request.edits);
        config.compact();
        Ok(config)
    }

    /// Collapses repeated edits to their last occurrence (`+mail -mail` stores one `-mail`).
    pub fn compact(&mut self) {
        let mut last: BTreeMap<String, usize> = BTreeMap::new();
        for (index, edit) in self.edits.iter().enumerate() {
            last.insert(edit_name(edit), index);
        }
        let mut kept = Vec::with_capacity(self.edits.len());
        for (index, edit) in self.edits.drain(..).enumerate() {
            if last.get(&edit_name(&edit)) == Some(&index) {
                kept.push(edit);
            }
        }
        self.edits = kept;
    }

    /// Whether this is exactly the shipped default (built-in base, no edits).
    #[must_use]
    pub fn is_default(&self) -> bool {
        !self.no_default && self.edits.is_empty()
    }

    /// Reads the optional `{features:[...], no_default_features:bool}` override one request carries.
    ///
    /// # Errors
    ///
    /// Returns the offending edit text when a request names something that is not a feature edit.
    pub fn from_params(params: &Value) -> Result<Option<Self>, String> {
        if params.get("features").is_none() && params.get("no_default_features").is_none() {
            return Ok(None);
        }
        let no_default = params
            .get("no_default_features")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut edits = Vec::new();
        for value in params
            .get("features")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let text = value
                .as_str()
                .ok_or_else(|| "each feature edit must be a string".to_owned())?;
            edits.push(text.parse::<FeatureEdit>()?);
        }
        Ok(Some(Self::new(no_default, edits)))
    }

    /// The canonical resolved set for `project`; never falls back to the document default.
    ///
    /// # Errors
    ///
    /// Returns the unreadable-file line or the compiler diagnostic naming an unknown feature.
    pub fn load_resolved(project: &Path, doc: &Forkfile) -> Result<FeatureSet, String> {
        Self::load(project)?.resolve(doc).map_err(|diagnostic| {
            format!(
                "Forkfile:{}: {}",
                diagnostic.span.line(),
                diagnostic.message
            )
        })
    }
}

/// The feature name behind one edit. `FeatureEdit`'s canonical render (`+mail`/`-mail`) is the
/// only public view of the name, so its one-character prefix is dropped here.
fn edit_name(edit: &FeatureEdit) -> String {
    let rendered = edit.to_string();
    rendered.get(1..).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use slipfork_forkfile::FeatureEdit;

    use super::WorldFeatures;

    fn temp(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    #[test]
    fn the_file_round_trips_and_an_absent_file_is_the_default() {
        let root = temp("slipfork-features-file");
        let project = root.path();
        assert!(
            WorldFeatures::load(project)
                .expect("an absent file is the default")
                .is_default()
        );
        assert!(!WorldFeatures::path(project).exists());

        let features = WorldFeatures::new(
            false,
            vec![
                FeatureEdit::Enable("mail".to_owned()),
                FeatureEdit::Disable("dev".to_owned()),
            ],
        );
        features.save(project).expect("save the features file");
        let text = fs::read_to_string(WorldFeatures::path(project)).expect("read features.json");
        assert!(text.contains("\"version\": 1"), "{text}");
        assert!(text.contains("\"+mail\""), "{text}");
        assert!(text.contains("\"-dev\""), "{text}");

        let loaded = WorldFeatures::load(project).expect("read the round-tripped file");
        assert_eq!(loaded, features);
        assert!(!loaded.is_default());

        fs::write(WorldFeatures::path(project), "not json").expect("corrupt the file");
        let malformed = WorldFeatures::load(project).expect_err("malformed JSON is an error");
        assert!(malformed.contains("features.json"), "{malformed}");
        assert!(malformed.contains("not valid features JSON"), "{malformed}");
    }

    #[test]
    fn merged_applies_request_edits_over_the_persisted_file_with_last_wins() {
        let root = temp("slipfork-features-merged");
        let project = root.path();
        WorldFeatures::new(
            false,
            vec![
                FeatureEdit::Enable("mail".to_owned()),
                FeatureEdit::Disable("dev".to_owned()),
            ],
        )
        .save(project)
        .expect("save the inherited file");

        let request = WorldFeatures::new(false, vec![FeatureEdit::Disable("mail".to_owned())]);
        let merged = WorldFeatures::merged(project, &request).expect("merge over the file");
        assert!(!merged.no_default, "the persisted base is kept");
        assert_eq!(
            merged.edits,
            vec![
                FeatureEdit::Disable("dev".to_owned()),
                FeatureEdit::Disable("mail".to_owned())
            ],
            "the later edit replaces the earlier one for the same name"
        );

        let reset = WorldFeatures::new(true, vec![FeatureEdit::Enable("dev".to_owned())]);
        let reset = WorldFeatures::merged(project, &reset).expect("explicit reset of the base");
        assert!(reset.no_default);
        assert_eq!(reset.edits, vec![FeatureEdit::Enable("dev".to_owned())]);

        let absent = temp("slipfork-features-absent");
        assert_eq!(
            WorldFeatures::merged(absent.path(), &WorldFeatures::default())
                .expect("an absent file is the default"),
            WorldFeatures::default()
        );

        fs::write(WorldFeatures::path(project), "not json").expect("corrupt the file");
        assert!(
            WorldFeatures::merged(project, &WorldFeatures::default()).is_err(),
            "a corrupt file never silently becomes the default"
        );
    }

    #[test]
    fn a_request_override_parses_only_the_edits_it_carries() {
        let none = serde_json::json!({ "project": "/tmp/demo" });
        assert_eq!(WorldFeatures::from_params(&none), Ok(None));

        let value =
            serde_json::json!({ "features": ["+mail", "-dev"], "no_default_features": true });
        let parsed = WorldFeatures::from_params(&value)
            .expect("parse the override")
            .expect("an override is present");
        assert!(parsed.no_default);
        assert_eq!(parsed.edits.len(), 2);

        let bad = serde_json::json!({ "features": ["--mail"] });
        assert!(WorldFeatures::from_params(&bad).is_err());
    }
}
