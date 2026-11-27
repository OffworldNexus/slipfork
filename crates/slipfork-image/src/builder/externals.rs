//! `COPY --from` resolution: a stage index, a declared alias, or an external image reference.
//!
//! The builder never pulls: the composition root pre-scans a `Dockerfile` with
//! [`external_references`], pulls every external rootfs, and hands the result back through
//! [`super::BuildRequest::externals`]. Keeping the classification here means stage syntax and
//! image-reference grammar never get re-scanned downstream.

use std::path::PathBuf;

use crate::dockerfile::{DockerfileParser, Instruction, Stage};
use crate::error::{Error, Result};
use crate::reference::ImageReference;
use crate::step_log::StepPlace;

/// A rootfs the composition root pulled for one `COPY --from=<image>` reference.
#[derive(Debug, Clone)]
pub struct ExternalImage {
    /// Read-only rootfs the external reference was unpacked into.
    pub rootfs: PathBuf,
    /// `sha256:<hex>` of the resolved manifest, part of the sealed step key.
    pub digest: String,
}

/// One `--from=<image>` reference a build needs pulled before it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalFrom {
    /// The reference exactly as the `Dockerfile` wrote it.
    pub reference: String,
    /// The `COPY`/`ADD` instruction's real index in the walked build, so its pre-build pull row
    /// never resets the boot tally or declares the image finished early.
    pub place: StepPlace,
}

/// Where one `COPY --from=<value>` reads its rootfs from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FromSource {
    /// The value is a numeric stage index.
    Index(usize),
    /// The value names a declared `AS` alias (case-insensitive).
    Stage(usize),
    /// The value is an OCI image reference the composition root must pull.
    External,
}

/// Classifies one `--from=<value>`: index first, then any declared alias, then an image ref.
///
/// A numeric value is always an index, never an image tag. Alias matching ignores ASCII case
/// because aliases are stored lowercased; external references keep their case verbatim.
pub(crate) fn classify_from(from: &str, stages: &[Stage]) -> Option<FromSource> {
    if let Ok(index) = from.parse::<usize>() {
        return Some(FromSource::Index(index));
    }
    if let Some(index) = stages.iter().position(|stage| {
        stage
            .alias
            .as_deref()
            .is_some_and(|alias| alias.eq_ignore_ascii_case(from))
    }) {
        return Some(FromSource::Stage(index));
    }
    ImageReference::parse(from)
        .ok()
        .map(|_| FromSource::External)
}

/// Lists the external image references the walked stages need, in declaration order.
///
/// Only `COPY`/`ADD` instructions inside stages up to `--target` are scanned, and each distinct
/// reference appears once at the line of its first use.
pub fn external_references(source: &str, target: Option<&str>) -> Result<Vec<ExternalFrom>> {
    let stages = DockerfileParser::parse(source)?;
    let walk = super::stage_walk(&stages, target)?;
    // Match `BuildRun`'s counter exactly: every walked stage contributes one `FROM` row plus its
    // instructions, and aliases are classified against this same walked prefix.
    let walked = &stages[..=walk.end];
    let total = walked
        .iter()
        .map(|stage| 1 + stage.instructions.len())
        .sum();
    let mut index = 0usize;
    let mut seen: Vec<&str> = Vec::new();
    let mut references = Vec::new();
    for stage in walked {
        index += 1;
        for parsed in &stage.instructions {
            index += 1;
            let (Instruction::Copy(spec) | Instruction::Add(spec)) = &parsed.instruction else {
                continue;
            };
            let Some(from) = spec.from.as_deref() else {
                continue;
            };
            if !matches!(classify_from(from, walked), Some(FromSource::External)) {
                continue;
            }
            if seen.contains(&from) {
                continue;
            }
            seen.push(from);
            references.push(ExternalFrom {
                reference: from.to_owned(),
                place: StepPlace::new(index, total, parsed.line),
            });
        }
    }
    Ok(references)
}

impl ExternalFrom {
    /// Folds a failed external pull into the `COPY` instruction that needed it.
    #[must_use]
    pub fn pull_failure(&self, error: &Error) -> Error {
        Error::StepFailed {
            line: self.place.line,
            phase: "copy",
            instruction: format!("--from={}", self.reference).into(),
            message: format!("pull `{}` failed: {}", self.reference, error.cause()),
            exit_code: None,
            runtime: None,
            next: error
                .remedy()
                .unwrap_or_else(|| "check the reference and retry".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FromSource, classify_from, external_references};
    use crate::dockerfile::DockerfileParser;
    use crate::step_log::StepPlace;

    /// The owner's real `Dockerfile`: one external `uv` rootfs, fetches and base stages ignored.
    const OWNER: &str = "FROM python:3.14-trixie AS base\n\
                         COPY --from=ghcr.io/astral-sh/uv:latest /uv /uvx /bin/\n\
                         ADD https://offworld-nexus.app/install.sh /tmp/oni.sh\n";

    #[test]
    fn owner_dockerfile_reports_exactly_the_uv_reference() {
        let references = external_references(OWNER, None).expect("the Dockerfile parses");
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].reference, "ghcr.io/astral-sh/uv:latest");
        // Real walked counter: FROM=1, COPY=2, ADD=3 ⇒ total 3.
        assert_eq!(references[0].place, StepPlace::new(2, 3, 2));
    }

    #[test]
    fn declared_stages_and_indices_are_never_pulled() {
        for dockerfile in [
            "FROM python:3.14-trixie AS base\nCOPY --from=base /x /y\n",
            "FROM python:3.14-trixie AS base\nCOPY --from=0 /x /y\n",
            "FROM python:3.14-trixie AS base\nCOPY --from= /x /y\n",
            "FROM python:3.14-trixie AS base\nCOPY --from=bad//ref /x /y\n",
        ] {
            let references = external_references(dockerfile, None).expect("the Dockerfile parses");
            assert!(references.is_empty(), "unexpected: {references:?}");
        }
    }

    #[test]
    fn a_reference_past_the_target_is_never_pulled() {
        let dockerfile = "FROM python:3.14-trixie AS base\n\
                          FROM ghcr.io/astral-sh/uv:latest AS tools\n\
                          COPY --from=ghcr.io/astral-sh/uv:latest /uv /bin/uv\n";
        let references =
            external_references(dockerfile, Some("base")).expect("the Dockerfile parses");
        assert!(references.is_empty(), "unexpected: {references:?}");
    }

    /// An alias declared past `--target` is not a stage of the walked prefix, so a `--from` naming
    /// it must be pulled as an external instead of failing later as `ExternalImageMissing`.
    #[test]
    fn an_alias_declared_past_the_target_is_pulled() {
        let dockerfile = "FROM python:3.14-trixie AS base\n\
                          COPY --from=tools /x /y\n\
                          FROM alpine AS tools\n";
        let references =
            external_references(dockerfile, Some("base")).expect("the Dockerfile parses");
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].reference, "tools");
    }

    #[test]
    fn classification_prefers_index_then_alias_then_reference() {
        let stages =
            DockerfileParser::parse("FROM alpine AS build\nFROM alpine\n").expect("parses");
        assert_eq!(classify_from("0", &stages), Some(FromSource::Index(0)));
        assert_eq!(classify_from("BUILD", &stages), Some(FromSource::Stage(0)));
        assert_eq!(
            classify_from("ghcr.io/astral-sh/uv:latest", &stages),
            Some(FromSource::External)
        );
        assert_eq!(classify_from("", &stages), None);
    }
}
