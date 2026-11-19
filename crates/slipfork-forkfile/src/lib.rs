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

mod argv;
mod check;
mod commands;
mod condition;
mod diag;
mod eval;
mod expr;
mod ir;
mod lexer;
mod lookup;
mod migrate;
mod parser;
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
pub use ir::{
    EvaluatedEnvGroup, EvaluatedFork, EvaluatedImage, EvaluatedPg, EvaluatedPgDatabase,
    EvaluatedRedis, EvaluatedService, EvaluatedTask, Facts, FactsValue, HookRun, PropertyMap,
    Resolvers, TaintedString,
};
pub use migrate::{MigrateError, migrate_snow_yaml};
pub use parser::parse_forkfile;
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
