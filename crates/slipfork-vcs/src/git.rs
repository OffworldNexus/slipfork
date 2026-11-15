//! `GitWorktreeAdapter`: `.worktrees/<fork>` creation plus the synthesized `<fork>/wip` commit
//! computed through a scratch `GIT_INDEX_FILE` under `.slipfork/git/`.
//!
//! Every Git invocation is an explicit `Command` argument vector (never a shell string) and the live
//! `.git/index` is never opened, renamed, or locked.
