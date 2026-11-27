//! Canonical digest of one image's on-disk build inputs, shared by the builder, stamps, and `up`.
//!
//! The digest folds the `Dockerfile` bytes and the `.dockerignore`-filtered build-context hash the
//! builder already keys every step on. It is the single owner of "did this image's inputs change",
//! so the daemon never re-walks a context with rules of its own.

use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::builder::stage_walk;
use crate::context::BuildContext;
use crate::digest::hex;
use crate::dockerfile::{BuildArgs, DockerfileParser, Instruction, Stage};
use crate::error::{Error, Result};

/// Whether evaluating `stages` reads the host build context (`COPY`/`ADD` without `--from`).
pub(crate) fn needs_context(stages: &[Stage]) -> bool {
    stages
        .iter()
        .flat_map(|stage| &stage.instructions)
        .any(|parsed| {
            matches!(
                &parsed.instruction,
                Instruction::Copy(spec) | Instruction::Add(spec) if spec.from.is_none()
            )
        })
}

/// The canonical content digest of one image's build inputs.
///
/// The `Dockerfile` bytes and the resolved build `arg` values always count. The context is only
/// walked when the walked stages have a host `COPY`/`ADD`, exactly like the builder, so editing
/// bind-mounted app sources never invalidates an image whose `Dockerfile` copies nothing from the
/// context. A service port or domain folded into an `arg` moves the digest even when no file moves.
///
/// # Errors
///
/// Propagates a missing/unreadable `Dockerfile`, a parse error, or a context walk failure.
pub fn build_inputs_digest(
    context: &Path,
    dockerfile: &Path,
    target: Option<&str>,
    args: &BuildArgs,
) -> Result<String> {
    let source =
        fs::read_to_string(dockerfile).map_err(|source| Error::io("read", dockerfile, source))?;
    let stages = DockerfileParser::parse(&source)?;
    let walk = stage_walk(&stages, target)?;
    let context_hash =
        BuildContext::for_walk(context, needs_context(&stages[..=walk.end]))?.hash()?;
    let mut hasher = Sha256::new();
    hasher.update(b"slipfork-build-inputs-v1\0");
    hasher.update((source.len() as u64).to_be_bytes());
    hasher.update(source.as_bytes());
    hasher.update(b"\0");
    hasher.update(b"\0args\0");
    hasher.update(args.fingerprint().as_bytes());
    hasher.update(context_hash.as_bytes());
    Ok(hex(&hasher.finalize()))
}
