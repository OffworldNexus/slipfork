//! Typed accessors over the parsed upstream tree.
//!
//! Every "missing key / wrong shape" refusal the `snow.yml` reader can emit is produced here, next
//! to the [`Node`] it inspects, so [`super::snow`] reads as the document model and nothing else.

use super::{MigrateError, schema};
use crate::yaml::Node;

/// Keyed reads that turn a structural mismatch into a `path`-aware refusal.
pub(super) trait Access {
    /// The named sequence, treating an absent key or an empty scalar as `[]`.
    fn sequence(&self, key: &str) -> Result<&[Node], MigrateError>;
    /// The named sequence, or `[]` whenever the key is absent.
    fn items(&self, key: &str) -> &[Node];
    /// A required scalar, refused as `<path>.<key>` when absent.
    fn scalar(&self, key: &str, path: &str) -> Result<String, MigrateError>;
    /// An optional scalar; an absent key reads as `None`.
    fn text(&self, key: &str) -> Option<String>;
    /// An optional `true`/`false` literal, refused as `<path>.<key>` for anything else.
    fn boolean(&self, key: &str, path: &str) -> Result<Option<bool>, MigrateError>;
    /// A sequence of scalars, refused as `<key>` when an entry is a map or a sequence.
    fn strings(&self, key: &str) -> Result<Vec<String>, MigrateError>;
    /// A sequence of ids (`- name` or `- id: name`), refused as `<path>[i]` otherwise.
    fn ids(&self, key: &str, path: &str) -> Result<Vec<String>, MigrateError>;
}

impl Access for Node {
    fn sequence(&self, key: &str) -> Result<&[Node], MigrateError> {
        match self.get(key) {
            Some(Node::Seq(nodes)) => Ok(nodes),
            Some(Node::Scalar(value)) if value.is_empty() => Ok(&[]),
            Some(_) => Err(schema(
                key,
                "expected a sequence",
                "write the entries as `- key: value` items",
            )),
            None => Ok(&[]),
        }
    }

    fn items(&self, key: &str) -> &[Node] {
        self.get(key).and_then(Node::seq).unwrap_or(&[])
    }

    fn scalar(&self, key: &str, path: &str) -> Result<String, MigrateError> {
        self.get(key)
            .and_then(Node::as_scalar)
            .map(str::to_owned)
            .ok_or_else(|| {
                schema(
                    format!("{path}.{key}"),
                    format!("missing `{key}`"),
                    format!("declare `{key}`"),
                )
            })
    }

    fn text(&self, key: &str) -> Option<String> {
        self.get(key).and_then(Node::as_scalar).map(str::to_owned)
    }

    fn boolean(&self, key: &str, path: &str) -> Result<Option<bool>, MigrateError> {
        let Some(value) = self.get(key) else {
            return Ok(None);
        };
        let Some(value) = value.as_scalar().map(str::trim) else {
            return Err(schema(
                format!("{path}.{key}"),
                "expected `true` or `false`",
                "write a boolean literal",
            ));
        };
        match value {
            "true" => Ok(Some(true)),
            "false" => Ok(Some(false)),
            other => Err(schema(
                format!("{path}.{key}"),
                format!("expected `true` or `false`, got `{other}`"),
                "write `pool: false` (or `true`)",
            )),
        }
    }

    fn strings(&self, key: &str) -> Result<Vec<String>, MigrateError> {
        self.items(key)
            .iter()
            .map(|entry| {
                entry.as_scalar().map(str::to_owned).ok_or_else(|| {
                    schema(key, "expected a string list", "write the values as scalars")
                })
            })
            .collect()
    }

    fn ids(&self, key: &str, path: &str) -> Result<Vec<String>, MigrateError> {
        id_list(self.items(key), path)
    }
}

/// The named sequence under an optional parent (`resources`), or `[]` when it is absent.
pub(super) fn parent_sequence<'a>(
    parent: Option<&'a Node>,
    key: &str,
) -> Result<&'a [Node], MigrateError> {
    match parent {
        Some(node) => node.sequence(key),
        None => Ok(&[]),
    }
}

/// The named sequence of ids under an optional parent (`resources`).
pub(super) fn parent_ids(
    parent: Option<&Node>,
    key: &str,
    path: &str,
) -> Result<Vec<String>, MigrateError> {
    match parent {
        Some(node) => node.ids(key, path),
        None => Ok(Vec::new()),
    }
}

fn id_list(nodes: &[Node], path: &str) -> Result<Vec<String>, MigrateError> {
    nodes
        .iter()
        .enumerate()
        .map(|(index, node)| match node {
            Node::Scalar(id) => Ok(id.clone()),
            Node::Map(_) => node.scalar("id", &format!("{path}[{index}]")),
            Node::Seq(_) => Err(schema(
                format!("{path}[{index}]"),
                "expected a bucket/domain map or id",
                "write `- id: <name>`",
            )),
        })
        .collect()
}
