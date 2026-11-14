//! `slipfork-snow`: `snow.yml` v2 parser, `modes` inheritance (`modes.all` + `modes.<mode>`),
//! `Faceless` v2 template evaluation (`??` fallback vs `!!` write-once secret generation),
//! and `SecretProvider` / `EntropySource` Dependency Injection contracts.

#![forbid(unsafe_code)]

pub mod conformance;
pub mod faceless;
pub mod model;
pub mod provider;

pub use faceless::{GeneratorSpec, parse_faceless, render_template};
pub use model::{
    BuildImage, Component, Domain, Hook, HookPhase, Image, Ingress, IngressMatch, IngressTarget,
    MissingRef, ModeSpec, PgCluster, PgDatabase, RedisCluster, ResolvedEnvMap, ResourceCoordinates,
    Resources, S3Bucket, SnowError, SnowFile,
};
pub use provider::{
    EntropySource, FileVaultProvider, LayeredSecretProvider, MemorySecretProvider, Namespace,
    OsCsprngEntropy, OsppBundle, ProviderError, SecretProvider, SecretValue,
};
