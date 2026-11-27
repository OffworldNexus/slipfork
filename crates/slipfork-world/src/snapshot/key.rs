//! Content-addressed node keys (`specs/03-under-the-hood.md` §6.1).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::SnapshotError;

/// Domain separator: bumping it re-keys every node in every store.
pub const KEY_DOMAIN: &[u8] = b"slipfork-node-v2\0";

/// What a node in the snapshot graph represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    /// A `FROM` image rootfs.
    Base,
    /// A `COPY --from` image rootfs.
    External,
    /// One `RUN`/`COPY`/`ADD` filesystem step.
    Step,
    /// An image's final config (record only, no subvolume).
    Image,
    /// A `PGDATA` `initdb` seed.
    Seed,
    /// A boot effect (lease entry).
    Effect,
    /// A long-running service (lease entry).
    Service,
    /// The world head (lease `head`).
    World,
}

impl NodeKind {
    /// Lowercase wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::External => "external",
            Self::Step => "step",
            Self::Image => "image",
            Self::Seed => "seed",
            Self::Effect => "effect",
            Self::Service => "service",
            Self::World => "world",
        }
    }

    /// Parses the lowercase wire name.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        [
            Self::Base,
            Self::External,
            Self::Step,
            Self::Image,
            Self::Seed,
            Self::Effect,
            Self::Service,
            Self::World,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == raw)
    }

    /// Whether the kind owns a sealed Btrfs subvolume in `<store>/snapshots/`.
    #[must_use]
    pub const fn is_materialized(self) -> bool {
        matches!(self, Self::Base | Self::External | Self::Step | Self::Seed)
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A 32-byte node digest used as both filename and graph identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct NodeKey([u8; 32]);

impl NodeKey {
    /// Wraps an already-computed SHA-256 digest.
    #[must_use]
    pub const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    /// Lowercase hex, the on-disk name.
    #[must_use]
    pub fn as_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// The derived key for one graph node.
    ///
    /// Locked grammar reading: parents are declared-order hex joined with `,`; inputs are
    /// `name=value` sorted by name and **each pair terminated by NUL**, appended after the final
    /// `\0` that closes `canon`. Secrets never enter a key here — callers pass compiled values.
    #[must_use]
    pub fn derive(
        kind: NodeKind,
        arch: &str,
        parents: &[NodeKey],
        canon: &str,
        inputs: &[(String, String)],
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(KEY_DOMAIN);
        hasher.update(kind.as_str().as_bytes());
        hasher.update([0]);
        hasher.update(arch.as_bytes());
        hasher.update([0]);
        let joined = parents
            .iter()
            .map(NodeKey::as_hex)
            .collect::<Vec<_>>()
            .join(",");
        hasher.update(joined.as_bytes());
        hasher.update([0]);
        hasher.update(normalize_canon(canon).as_bytes());
        hasher.update([0]);
        let mut pairs: Vec<(&str, &str)> = inputs
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        pairs.sort_by(|left, right| left.0.cmp(right.0));
        for (name, value) in pairs {
            hasher.update(name.as_bytes());
            hasher.update(b"=");
            hasher.update(value.as_bytes());
            hasher.update([0]);
        }
        Self(hasher.finalize().into())
    }

    /// A `FROM` image node.
    #[must_use]
    pub fn base(arch: &str, manifest: &str, platform: &str) -> Self {
        Self::derive(
            NodeKind::Base,
            arch,
            &[],
            &format!("FROM {manifest}"),
            &[
                ("manifest".to_owned(), manifest.to_owned()),
                ("platform".to_owned(), platform.to_owned()),
            ],
        )
    }

    /// A `COPY --from` image node.
    #[must_use]
    pub fn external(arch: &str, manifest: &str) -> Self {
        Self::derive(
            NodeKind::External,
            arch,
            &[],
            &format!("COPY --from {manifest}"),
            &[("manifest".to_owned(), manifest.to_owned())],
        )
    }

    /// One `RUN`/`COPY` step over its previous step plus in-scope `ARG` values.
    #[must_use]
    pub fn step(
        arch: &str,
        parents: &[NodeKey],
        canon: &str,
        config: &str,
        args: &[(String, String)],
    ) -> Self {
        Self::step_with_inputs(arch, parents, canon, config, args, &[])
    }

    /// One step whose key also folds kind-specific inputs (`sources` for `COPY`/`ADD`).
    #[must_use]
    pub fn step_with_inputs(
        arch: &str,
        parents: &[NodeKey],
        canon: &str,
        config: &str,
        args: &[(String, String)],
        extra: &[(String, String)],
    ) -> Self {
        let mut inputs = vec![("config".to_owned(), config.to_owned())];
        inputs.extend(args.iter().cloned());
        inputs.extend(extra.iter().cloned());
        Self::derive(NodeKind::Step, arch, parents, canon, &inputs)
    }

    /// An image's final config node (record only).
    #[must_use]
    pub fn image(arch: &str, parent: &NodeKey, canon: &str, config_digest: &str) -> Self {
        Self::derive(
            NodeKind::Image,
            arch,
            &[*parent],
            canon,
            &[("config".to_owned(), config_digest.to_owned())],
        )
    }

    /// A `seed` node over the `postgres` image it `initdb`s from.
    #[must_use]
    pub fn seed(
        arch: &str,
        image: &NodeKey,
        initdb_argv: &[String],
        template_version: &str,
    ) -> Self {
        Self::derive(
            NodeKind::Seed,
            arch,
            &[*image],
            "seed",
            &[
                ("initdb".to_owned(), initdb_argv.join(" ")),
                ("template_version".to_owned(), template_version.to_owned()),
            ],
        )
    }

    /// A boot effect lease entry over its image.
    #[must_use]
    pub fn effect(
        arch: &str,
        image: &NodeKey,
        command: &str,
        phase: &str,
        slot: &str,
        when: &str,
    ) -> Self {
        Self::derive(
            NodeKind::Effect,
            arch,
            &[*image],
            command,
            &[
                ("phase".to_owned(), phase.to_owned()),
                ("slot".to_owned(), slot.to_owned()),
                ("when".to_owned(), when.to_owned()),
            ],
        )
    }

    /// A service lease entry over its image.
    #[must_use]
    pub fn service(arch: &str, image: &NodeKey, canon: &str, spec: &str) -> Self {
        Self::derive(
            NodeKind::Service,
            arch,
            &[*image],
            canon,
            &[("spec".to_owned(), spec.to_owned())],
        )
    }

    /// The world head over every seed/effect/service entry, in sorted parent order.
    ///
    /// The world node is host-agnostic: `spec` already carries the platform, so `arch` is empty.
    #[must_use]
    pub fn world(parents: &[NodeKey], spec: &str) -> Self {
        let mut sorted = parents.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        Self::derive(
            NodeKind::World,
            "",
            &sorted,
            "world",
            &[("spec".to_owned(), spec.to_owned())],
        )
    }
}

impl fmt::Display for NodeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_hex())
    }
}

impl FromStr for NodeKey {
    type Err = SnapshotError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let canonical = value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !canonical {
            return Err(SnapshotError::Key(value.to_owned()));
        }
        let mut digest = [0u8; 32];
        hex::decode_to_slice(value, &mut digest)
            .map_err(|_| SnapshotError::Key(value.to_owned()))?;
        Ok(Self(digest))
    }
}

impl From<NodeKey> for String {
    fn from(key: NodeKey) -> Self {
        key.as_hex()
    }
}

impl TryFrom<String> for NodeKey {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse::<Self>().map_err(|error| error.to_string())
    }
}

/// Canonical text: ASCII whitespace runs collapse to one space and the edges are trimmed.
#[must_use]
pub fn normalize_canon(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_ascii_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> NodeKey {
        NodeKey::from_digest([byte; 32])
    }

    #[test]
    fn grammar_vector_is_exact() {
        let inputs = [
            ("b".to_owned(), "22".to_owned()),
            ("a".to_owned(), "1".to_owned()),
        ];
        let derived = NodeKey::derive(
            NodeKind::Step,
            "amd64",
            &[key(7)],
            "RUN  echo   hi\n",
            &inputs,
        );
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"slipfork-node-v2\0");
        bytes.extend_from_slice(b"step\0");
        bytes.extend_from_slice(b"amd64\0");
        bytes.extend_from_slice(key(7).as_hex().as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(b"RUN echo hi\0");
        bytes.extend_from_slice(b"a=1\0b=22\0");
        let expected: [u8; 32] = Sha256::digest(&bytes).into();
        assert_eq!(derived, NodeKey::from_digest(expected));
        assert_eq!(derived.as_hex().len(), 64);
        assert_eq!(derived.as_hex(), derived.as_hex().to_ascii_lowercase());
    }

    #[test]
    fn kind_parents_and_inputs_order_are_sensitive() {
        let (a, b) = (key(1), key(2));
        let inputs = [
            ("k".to_owned(), "v".to_owned()),
            ("z".to_owned(), "y".to_owned()),
        ];
        let reversed = [inputs[1].clone(), inputs[0].clone()];
        let base = NodeKey::derive(NodeKind::Step, "amd64", &[a, b], "RUN x", &inputs);
        assert_eq!(
            base,
            NodeKey::derive(NodeKind::Step, "amd64", &[a, b], "RUN x", &reversed),
            "inputs are sorted by name, so declaration order never matters"
        );
        assert_ne!(
            base,
            NodeKey::derive(NodeKind::Image, "amd64", &[a, b], "RUN x", &inputs)
        );
        assert_ne!(
            base,
            NodeKey::derive(NodeKind::Step, "amd64", &[b, a], "RUN x", &inputs)
        );
        assert_ne!(
            base,
            NodeKey::derive(NodeKind::Step, "arm64", &[a, b], "RUN x", &inputs)
        );
    }

    #[test]
    fn canon_whitespace_normalizes() {
        assert_eq!(normalize_canon("  RUN\techo\n\n hi  "), "RUN echo hi");
        assert_eq!(normalize_canon("RUN echo hi"), "RUN echo hi");
    }

    #[test]
    fn seed_key_tracks_version_argv_and_image() {
        let image = NodeKey::base("amd64", "sha256:1", "linux/amd64");
        let argv = vec!["initdb".to_owned(), "-D".to_owned(), "data".to_owned()];
        let seed = NodeKey::seed("amd64", &image, &argv, "v1");
        assert_eq!(seed, NodeKey::seed("amd64", &image, &argv, "v1"));
        assert_ne!(seed, NodeKey::seed("amd64", &image, &argv, "v2"));
        assert_ne!(
            seed,
            NodeKey::seed("amd64", &image, &["initdb".to_owned()], "v1")
        );
        assert_ne!(
            seed,
            NodeKey::seed(
                "amd64",
                &NodeKey::base("amd64", "sha256:2", "linux/amd64"),
                &argv,
                "v1"
            )
        );
    }
}
