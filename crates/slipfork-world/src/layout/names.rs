//! Validated world and fork names shared by every store and project path.

use std::fmt;

use crate::error::{Error, NameKind, Result};

/// A validated `worlds/<world>` data subvolume name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorldName(String);

impl WorldName {
    /// Parses `raw` (`[a-z0-9][a-z0-9._-]*`) into a world name.
    pub fn parse(raw: &str) -> Result<Self> {
        validate_name(NameKind::World, raw).map(Self)
    }

    /// Wraps an already-validated world id composed inside this crate.
    pub(crate) fn new(raw: String) -> Self {
        Self(raw)
    }

    /// The validated name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorldName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A validated fork name shared by both snapshots of a fork.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ForkName(String);

impl ForkName {
    /// Parses `raw` (`[a-z0-9][a-z0-9._-]*`) into a fork name.
    pub fn parse(raw: &str) -> Result<Self> {
        validate_name(NameKind::Fork, raw).map(Self)
    }

    /// Adopts an already-split store entry half verbatim.
    ///
    /// Legacy nested `<fork>--<parent>--<root>` entries predate the reserved `--` separator, so
    /// [`crate::StoreLayout::parse_fork_world`] accepts their fork half while [`ForkName::parse`]
    /// keeps `--` reserved for every new name.
    pub(crate) fn from_entry(raw: &str) -> Option<Self> {
        validate(NameKind::Fork, raw, true).ok().map(Self)
    }

    /// The validated name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ForkName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub(crate) fn validate_name(kind: NameKind, raw: &str) -> Result<String> {
    validate(kind, raw, false)
}

/// The one name validator; `allow_separator` admits legacy store entries whose fork half still
/// carries the reserved `--` separator.
fn validate(kind: NameKind, raw: &str, allow_separator: bool) -> Result<String> {
    let mut bytes = raw.bytes();
    let starts_well = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let rest_well = bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    });
    // Fork world ids are `<fork>--<root>` (`specs/03` §7.3). Reserving `--` inside a fork name
    // keeps that separator unambiguous for every depth, with world/pod names unaffected.
    let reserved = kind == NameKind::Fork && !allow_separator && raw.contains("--");
    if !starts_well || !rest_well || reserved || matches!(raw, "." | "..") {
        return Err(Error::InvalidName {
            kind,
            name: raw.to_owned(),
        });
    }
    Ok(raw.to_owned())
}
