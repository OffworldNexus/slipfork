//! The `DaemonControl` protocol surface: one explicit forwarder per verb.
//!
//! Every body lives in its cohesive owner module (`boot`, `world`, `backing`, `protocol`, `ospp`,
//! `resolve`, `service`); each forwarder calls that inherent method by UFCS, so the trait impl
//! stays a thin adapter and no verb body is duplicated here.

use super::service::DaemonService;
use slipfork_edge::{DaemonControl, EdgeError};
use slipfork_wire::control::{
    BootEvent, DaemonStatus, DropForkRequest, ExecTargetRequest, ForkRequest, ForwardRequest,
    ForwardSummary, HookRunRequest, LogsRequest, PruneReport, UpRequest, WorkspaceRefsRequest,
    WorkspaceRefsSummary, WorldExecTarget, WorldProcessReport, WorldSelector, WorldSummary,
    WorldUpSummary,
};
use slipfork_world::{LogChunk, StreamChunk};
use std::path::Path;

impl DaemonControl for DaemonService {
    fn status(&self) -> Result<DaemonStatus, EdgeError> {
        DaemonService::status(self)
    }

    fn up(
        &self,
        req: &UpRequest,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError> {
        DaemonService::up(self, req, on_event)
    }

    fn restart(
        &self,
        req: &slipfork_wire::control::RestartRequest,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError> {
        DaemonService::restart(self, req, on_event)
    }

    fn down(&self, req: &WorldSelector) -> Result<bool, EdgeError> {
        DaemonService::down(self, req)
    }

    fn fork(
        &self,
        req: &ForkRequest,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError> {
        DaemonService::fork(self, req, on_event)
    }

    fn drop_fork(&self, req: &DropForkRequest) -> Result<bool, EdgeError> {
        DaemonService::drop_fork(self, req)
    }

    fn list(&self, repo_root: Option<&Path>) -> Result<Vec<WorldSummary>, EdgeError> {
        DaemonService::list(self, repo_root)
    }

    fn world_processes(&self, req: &WorldSelector) -> Result<WorldProcessReport, EdgeError> {
        DaemonService::world_processes(self, req)
    }

    fn prune(&self, dry_run: bool) -> Result<PruneReport, EdgeError> {
        DaemonService::prune(self, dry_run)
    }

    fn secret_set(
        &self,
        repo_root: &Path,
        key: &str,
        value: &str,
        force: bool,
    ) -> Result<(), EdgeError> {
        DaemonService::secret_set(self, repo_root, key, value, force)
    }

    fn secret_unset(&self, repo_root: &Path, key: &str) -> Result<bool, EdgeError> {
        DaemonService::secret_unset(self, repo_root, key)
    }

    fn secret_list(&self, repo_root: &Path) -> Result<Vec<String>, EdgeError> {
        DaemonService::secret_list(self, repo_root)
    }

    fn logs(
        &self,
        req: &LogsRequest,
        on_chunk: &mut dyn FnMut(LogChunk) -> std::ops::ControlFlow<()>,
    ) -> Result<(), EdgeError> {
        DaemonService::logs(self, req, on_chunk)
    }

    fn exec_target(
        &self,
        req: &ExecTargetRequest,
    ) -> Result<slipfork_wire::control::WorldExecTarget, EdgeError> {
        DaemonService::exec_target(self, req)
    }

    fn hook_exec_target(&self, req: &HookRunRequest) -> Result<WorldExecTarget, EdgeError> {
        DaemonService::hook_exec_target(self, req)
    }

    fn workspace_refs(
        &self,
        req: &WorkspaceRefsRequest,
    ) -> Result<WorkspaceRefsSummary, EdgeError> {
        DaemonService::workspace_refs(self, req)
    }

    fn hook_run(
        &self,
        req: &HookRunRequest,
        on_chunk: &mut dyn FnMut(StreamChunk),
    ) -> Result<i32, EdgeError> {
        DaemonService::hook_run(self, req, on_chunk)
    }

    fn forward(
        &self,
        req: &ForwardRequest,
        wait_disconnect: &mut dyn FnMut(ForwardSummary),
    ) -> Result<(), EdgeError> {
        DaemonService::forward(self, req, wait_disconnect)
    }

    fn shutdown(&self) -> Result<(), EdgeError> {
        DaemonService::shutdown(self)
    }

    fn auth_login(
        &self,
        endpoint_url: Option<&str>,
    ) -> Result<slipfork_wire::control::AuthSummary, EdgeError> {
        DaemonService::auth_login(self, endpoint_url)
    }
}
