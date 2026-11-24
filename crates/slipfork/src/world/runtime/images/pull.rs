//! Resource-image pulls: one OCI fetch per reference, reused by every pod built from it.

use slipfork_image::{ImageReference, PulledImage, StepLog};

use super::ImageCache;
use super::flight::{FlightRole, acquire_flight, wait_flight};
use crate::world::supervise::BootSink;

impl ImageCache {
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
