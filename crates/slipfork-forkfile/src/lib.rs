//! `slipfork-forkfile`: the single canonical owner of `<repo_root>/Forkfile`.
//!
//! The crate lexes and parses `Forkfile` into a span-carrying AST, statically checks **every**
//! statement and every `on <expr>` branch (active and inactive) in one pass, evaluates the active
//! Cargo-style [`FeatureSet`] into an [`EvaluatedFork`], resolves per-service environments through
//! injected [`Resolvers`], and round-trips the whole manifest through canonical JSON. It also owns
//! the local `0600` [`FileVaultProvider`] behind the [`SecretProvider`] / [`EntropySource`] ports.
//!
//! No `snow.yml` concept leaks through this surface: there are no modes, no `!!` markers, and no
//! YAML types. Library code never reads the process environment, the network, or the clock.

#![forbid(unsafe_code)]

/// The manifest feature every local world declares (`features { local }` / `on local`).
pub const WORLD_FEATURE: &str = "local";

mod argv;
mod check;
mod commands;
mod condition;
mod diag;
mod eval;
mod expr;
mod fmt;
mod hover;
mod ir;
mod lexer;
mod lookup;
mod migrate;
mod parser;
mod projection;
mod props;
mod resolve;
mod resources;
mod service;
mod syntax;
mod vault;
mod yaml;

pub use check::ExternalCheck;
pub use diag::{Diagnostic, Span};
pub use eval::FeatureSet;
pub use fmt::fmt;
pub use hover::{Word, hover, word_at};
pub use ir::{
    DeploymentFacts, EntityKind, EvaluatedEnvGroup, EvaluatedFork, EvaluatedImage, EvaluatedPg,
    EvaluatedPgDatabase, EvaluatedRedis, EvaluatedService, EvaluatedTask, HookRun, Resolvers,
    TaintedString,
};
pub use migrate::{MigrateError, migrate_snow_yaml};
pub use parser::parse_forkfile;
pub use projection::{BuildImage, IngressRoute, WorldFact, WorldPgDatabase};
pub use syntax::{
    ArgPart, ArgRef, ArgRefKind, Argv, ArgvSlot, Condition, DomainDef, EnvAssignment, EnvGroup,
    EnvPrefix, Expr, FeatureDef, ForkFile, GeneratorKind, HookPhase, ImageSource, OnBlock,
    PgClusterDef, PgDatabaseDef, PgOn, RedisClusterDef, RedisOn, S3Def, ServiceDef,
    ServiceDirective, Spanned, Stmt, TaskDef,
};
pub use vault::{
    EntropySource, FileVaultProvider, MutableSecretStore, Namespace, OsCsprngEntropy,
    ProviderError, SecretProvider,
};
