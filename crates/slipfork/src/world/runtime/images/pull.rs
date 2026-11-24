//! Resource-image pulls: one OCI fetch per reference, reused by every pod built from it.

use std::collections::BTreeMap;

use slipfork_image::{
    BuildRequest, ExternalImage, ImageReference, PulledImage, StepLog, external_references,
};

use super::ImageCache;
use super::flight::{FlightRole, acquire_flight, wait_flight};
use crate::world::supervise::BootSink;

impl ImageCache {
    /// Pre-scans the `Dockerfile` and pulls every external `COPY --from=<image>` rootfs.
    ///
    /// Each pull is one live `COPY` row at the instruction's own walked position and `Dockerfile`
    /// line; a failure folds back onto that line instead of surfacing a step-less build error.
    pub(super) fn pull_externals(
        &self,
        request: &BuildRequest,
        step_log: &mut StepLog<'_>,
    ) -> Result<BTreeMap<String, ExternalImage>, slipfork_image::Error> {
        let source = std::fs::read_to_string(&request.dockerfile).map_err(|source| {
            slipfork_image::Error::Io {
                action: "read",
                path: request.dockerfile.clone(),
                source,
            }
        })?;
        let references = external_references(&source, request.target.as_deref())?;
        let mut externals = BTreeMap::new();
        for external in references {
            step_log.announce_at(
                external.place,
                "COPY",
                &format!("--from={}", external.reference),
            );
            let started = std::time::Instant::now();
            let outcome = match ImageReference::parse(&external.reference) {
                Ok(parsed) => self.puller.pull_into(&parsed, step_log),
                Err(error) => Err(error),
            };
            match outcome {
                Ok(image) => {
                    step_log.done(started.elapsed());
                    externals.insert(
                        external.reference.clone(),
                        ExternalImage {
                            rootfs: image.rootfs,
                            digest: image.digest,
                        },
                    );
                }
                Err(error) => {
                    step_log.failed(started.elapsed(), error.exit_code());
                    return Err(external.pull_failure(&error));
                }
            }
        }
        Ok(externals)
    }

    /// Pulls one resource image (`postgres:18`, `redis:7`) once and reuses it afterwards.
    pub(in crate::world::runtime) fn pull(
        &self,
        reference: &str,
        log: &mut BootSink,
    ) -> Result<PulledImage, String> {
        if let Some(image) = self
            .pulled
            .lock()
            .map_err(|_| "pulled image lock poisoned".to_owned())?
            .get(reference)
            .cloned()
        {
            return Ok(image);
        }
        let guard = match acquire_flight(&self.inflight_pulls, reference.to_owned())? {
            FlightRole::Wait(flight) => return wait_flight(&flight),
            FlightRole::Lead(guard) => guard,
        };
        if let Some(image) = self
            .pulled
            .lock()
            .map_err(|_| "pulled image lock poisoned".to_owned())?
            .get(reference)
            .cloned()
        {
            let outcome = Ok(image);
            guard.finish(outcome.clone());
            return outcome;
        }
        let outcome = ImageReference::parse(reference)
            .map_err(|error| error.to_string())
            .and_then(|parsed| {
                let mut bus = |event: slipfork_image::BuildEvent<'_>| {
                    if let slipfork_image::BuildEvent::Note { text } = event {
                        log.engine_note(text);
                    }
                };
                self.puller
                    .pull_into(&parsed, &mut StepLog::with_events(&mut bus))
                    .map_err(|error| error.to_string())
            });
        if let Ok(image) = &outcome
            && let Ok(mut pulled) = self.pulled.lock()
        {
            pulled.insert(reference.to_owned(), image.clone());
        }
        guard.finish(outcome.clone());
        outcome
    }
}
