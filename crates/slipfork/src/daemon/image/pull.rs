//! `image.pull`: forget the stale ref, stream the live `FROM` row first, then refresh it.
//!
//! The remembered digest is dropped before the row starts, so the network wait is visible and
//! `pull_uncached` re-resolves the tag and remembers the fresh digest for the next build.

use serde_json::{Value, json};
use slipfork_image::{BuildEvent, ImageReference, StepLog};

use super::{ImageFrame, ImageLab, ImageRequest, client_closed, failure};
use crate::world::HostEnv;

/// Refreshes one OCI reference and streams its live `FROM` row as typed frames.
pub(crate) fn pull(
    host: &HostEnv,
    params: &Value,
    emit: &mut dyn FnMut(Value) -> bool,
) -> Result<Option<Value>, super::Failure> {
    let request = ImageRequest::parse(params)?;
    let reference = params
        .get("reference")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            failure(
                "bad-params",
                "`reference` is required",
                "pass a full reference such as `ghcr.io/owner/image:tag`",
            )
        })?
        .to_owned();
    let parsed = ImageReference::parse(&reference).map_err(|error| {
        failure(
            "invalid-reference",
            format!("invalid image reference `{reference}`: {}", error.cause()),
            "use a full reference such as `ghcr.io/owner/image:tag`",
        )
    })?;
    let lab = ImageLab::for_project(host, &request.project)?;
    // Removing the remembered digest keeps the row live and makes the next resolve authoritative.
    lab.forget_ref(&reference)?;
    let puller = lab.puller(host);
    let mut closed = false;
    let mut pump = |frame: ImageFrame| {
        if closed {
            return;
        }
        if !emit(frame.value()) {
            closed = true;
        }
    };
    let pulled = {
        let mut bus = |event: BuildEvent<'_>| pump(ImageFrame::from_event(event));
        puller.pull_row(&parsed, &reference, &mut StepLog::with_events(&mut bus))
    };
    if closed {
        return Err(client_closed());
    }
    let pulled = pulled.map_err(|error| {
        failure(
            "registry-pull",
            format!("pull `{parsed}` failed: {}", error.cause()),
            "check the reference and registry credentials, then retry",
        )
    })?;
    let digest = digest_label(&pulled.digest);
    Ok(Some(json!({
        "reference": parsed.to_string(),
        "digest": digest,
        "rootfs": pulled.rootfs.display().to_string(),
        "cached": pulled.cached,
        "exit": 0,
    })))
}

fn digest_label(value: &str) -> String {
    if value.starts_with("sha256:") {
        value.to_owned()
    } else {
        format!("sha256:{value}")
    }
}
