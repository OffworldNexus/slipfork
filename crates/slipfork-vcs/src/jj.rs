//! `JjWorkspaceAdapter`: Jujutsu workspaces mapping `@-` -> `refs/heads/<fork>` and `@` ->
//! `refs/slipfork/<fork>/wip` (collapsing to `@-`'s commit when `@` is empty/clean).
