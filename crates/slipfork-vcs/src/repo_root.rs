//! `RepoRootResolver` adapter: walks parent directories to the `.git` entry (file or directory) and
//! fails closed with `VcsError::NotARepository`.
