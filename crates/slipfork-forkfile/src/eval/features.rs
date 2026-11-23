//! Feature-set resolution: built-in `dev`, `features { }` groups, and ordered `+feat`/`-feat`
//! edits applied on top of the default base.
//!
//! The `on`-branch walk that indexes declared groups lives in [`crate::check::feature_index`]; this
//! module only expands implications and validates edits against the document.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::values::expand_feature;
use crate::ast::Forkfile;
use crate::diag::Diagnostic;
use crate::expr::{Expr, ExprKind};
use crate::span::Span;

/// An evaluated feature set: the active names plus every name the document makes available.
#[derive(Debug, Clone)]
pub struct FeatureSet {
    active: BTreeSet<String>,
    available: BTreeSet<String>,
}

impl FeatureSet {
    /// The active feature names.
    #[must_use]
    pub fn active(&self) -> &BTreeSet<String> {
        &self.active
    }

    /// Every feature name the document accepts (`dev` plus declared groups and features).
    #[must_use]
    pub fn available(&self) -> &BTreeSet<String> {
        &self.available
    }

    /// Whether `name` is active.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.active.contains(name)
    }

    /// Evaluates an `on <cond>` expression against the active names.
    #[must_use]
    pub fn matches(&self, cond: &Expr) -> bool {
        match &cond.kind {
            ExprKind::Bool(value) => *value,
            ExprKind::Feature(name) => self.active.contains(name),
            ExprKind::Not(inner) => !self.matches(inner),
            ExprKind::And(left, right) => self.matches(left) && self.matches(right),
            ExprKind::Or(left, right) => self.matches(left) || self.matches(right),
            _ => false,
        }
    }
}

/// One ordered feature edit applied on top of the default set (`+mail`, `-dev`, `!mail`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum FeatureEdit {
    /// `feat` or `+feat`.
    Enable(String),
    /// `-feat` or `!feat`.
    Disable(String),
}

impl FeatureEdit {
    pub(super) fn name(&self) -> &str {
        match self {
            Self::Enable(name) | Self::Disable(name) => name,
        }
    }

    pub(super) fn enables(&self) -> bool {
        matches!(self, Self::Enable(_))
    }
}

impl fmt::Display for FeatureEdit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Enable(name) => write!(formatter, "+{name}"),
            Self::Disable(name) => write!(formatter, "-{name}"),
        }
    }
}

impl FromStr for FeatureEdit {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        let (enable, name) = match text.as_bytes().first() {
            Some(b'+') => (true, &text[1..]),
            Some(b'-' | b'!') => (false, &text[1..]),
            _ => (true, text),
        };
        // A second prefix character (`--mail`, `+-dev`) is a malformed edit, not a feature name.
        if name.is_empty()
            || name.starts_with(['+', '-', '!'])
            || !name.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
        {
            return Err(format!("invalid feature edit `{text}`"));
        }
        Ok(if enable {
            Self::Enable(name.to_owned())
        } else {
            Self::Disable(name.to_owned())
        })
    }
}

impl From<FeatureEdit> for String {
    fn from(edit: FeatureEdit) -> Self {
        edit.to_string()
    }
}

impl TryFrom<String> for FeatureEdit {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

impl Forkfile {
    /// Every feature name this document accepts: built-in `dev` plus declared groups/features.
    #[must_use]
    pub fn available_features(&self) -> BTreeSet<String> {
        crate::check::available_features(&self.items.items)
    }

    /// The `features { … }` span for feature diagnostics, falling back to the header span.
    fn feature_span(&self) -> Span {
        crate::check::feature_index(&self.items.items)
            .1
            .unwrap_or(self.span)
    }

    /// Every feature group and its declared implications.
    fn feature_groups(&self) -> BTreeMap<String, Vec<String>> {
        crate::check::feature_index(&self.items.items).0
    }

    /// The default base: built-in `dev` (with its implications) plus `default`'s expansion.
    fn base_active(&self, groups: &BTreeMap<String, Vec<String>>) -> BTreeSet<String> {
        let mut active = BTreeSet::new();
        expand_feature("dev", groups, &mut active);
        if groups.contains_key("default") {
            expand_feature("default", groups, &mut active);
        }
        active
    }

    /// Resolves the default set (unless `no_default`) and applies `edits` in order.
    ///
    /// Enabling a name expands its declared implications transitively; disabling removes that
    /// name alone. Every name is validated against [`Forkfile::available_features`].
    ///
    /// # Errors
    ///
    /// Returns one diagnostic naming the unknown feature and the available set.
    pub fn resolve_features(
        &self,
        no_default: bool,
        edits: &[FeatureEdit],
    ) -> Result<FeatureSet, Diagnostic> {
        let available = self.available_features();
        let groups = self.feature_groups();
        let mut active = if no_default {
            BTreeSet::new()
        } else {
            self.base_active(&groups)
        };
        for edit in edits {
            let name = edit.name();
            if !available.contains(name) {
                return Err(Diagnostic::new(
                    self.feature_span(),
                    format!("unknown feature `{name}`"),
                    feature_remedy(&available),
                ));
            }
            if edit.enables() {
                expand_feature(name, &groups, &mut active);
            } else {
                active.remove(name);
            }
        }
        Ok(FeatureSet { active, available })
    }

    /// The default feature set: built-in `dev` plus `features { default = [...] }`.
    #[must_use]
    pub fn default_features(&self) -> FeatureSet {
        let groups = self.feature_groups();
        FeatureSet {
            active: self.base_active(&groups),
            available: self.available_features(),
        }
    }

    // Temporary delegator: delete in M3.69.2 once up.rs passes the resolved set.
    #[must_use]
    pub fn active_features(&self) -> FeatureSet {
        self.default_features()
    }
}

/// `next:` line for an unknown feature: up to six names, then `+N more`.
fn feature_remedy(available: &BTreeSet<String>) -> String {
    const CAP: usize = 6;
    let mut names = Vec::new();
    if available.contains("dev") {
        names.push("dev".to_owned());
    }
    names.extend(
        available
            .iter()
            .filter(|name| name.as_str() != "dev")
            .cloned(),
    );
    let shown = if names.len() > CAP {
        format!("{}, +{} more", names[..CAP].join(", "), names.len() - CAP)
    } else {
        names.join(", ")
    };
    format!("use one of {shown} or declare it in `features {{ }}`")
}
