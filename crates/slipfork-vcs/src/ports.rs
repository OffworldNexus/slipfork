//! Domain ports: `RepoRootResolver`, `VcsWorkspaceFactory`, `VcsWorkspace`, and their `RepoRoot` /
//! `WorkspaceRequest` / `WorkspaceRefs` / `WorkspaceChanges` value types.
//!
//! Zero `std::process`, zero `std::env`, zero clock reads: adapters own all ambient access, and no
//! `AdapterKind` discriminator exists outside `slipforkd`'s factory.
