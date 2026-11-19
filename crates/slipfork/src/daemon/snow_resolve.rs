//! `snow resolve` control verb: raw `snow.yml` values for the thin CLI.

use std::fs;

use slipfork_edge::EdgeError;
use slipfork_snow::{OsCsprngEntropy, ResourceCoordinates, SnowFile};
use slipfork_wire::control::{SnowResolveRequest, SnowResolveSummary};
use slipfork_world::WorldTarget;

use super::runtime_error;
use super::service::DaemonService;
use super::types::parent_dir;

impl DaemonService {
    pub(super) fn snow_resolve(
        &self,
        req: &SnowResolveRequest,
    ) -> Result<SnowResolveSummary, EdgeError> {
        let file = if req.file.is_absolute() {
            req.file.clone()
        } else {
            req.cwd.join(&req.file)
        };
        let raw = fs::read_to_string(&file)
            .map_err(|err| runtime_error(format!("cannot read {}: {err}", file.display())))?;
        let snow = SnowFile::parse_yaml(&raw)
            .map_err(|err| runtime_error(format!("{}: {err}", file.display())))?;
        let repository = self
            .repo_roots
            .resolve(&parent_dir(&file))
            .map_err(|err| runtime_error(err.to_string()))?;
        let project = repository.project;
        let vault = self.vault_for(&project)?;
        let coords = ResourceCoordinates::local_for_world(&snow, &project, &WorldTarget::Canonical)
            .map_err(|err| runtime_error(err.to_string()))?;
        let resolved = snow
            .resolve_mode(
                &req.mode,
                req.component.as_deref(),
                vault.as_ref(),
                &coords,
                &OsCsprngEntropy,
            )
            .map_err(|err| runtime_error(err.to_string()))?;
        let vars = resolved
            .iter()
            .map(|(key, value)| {
                let text = if req.expose_secrets {
                    value.expose()
                } else {
                    value.redacted()
                };
                (key.clone(), text.to_owned())
            })
            .collect();
        Ok(SnowResolveSummary { vars })
    }
}
