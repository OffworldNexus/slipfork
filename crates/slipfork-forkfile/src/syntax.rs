//! `Forkfile` AST: the canonical, span-carrying manifest IR that round-trips through JSON.

use serde::{Deserialize, Serialize};

use crate::diag::Span;

pub use crate::commands::{ArgPart, ArgRef, ArgRefKind, Argv, ArgvSlot, EnvPrefix, HookPhase};
pub use crate::condition::Condition;

/// A value carrying the span it was parsed from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spanned<T> {
    /// The parsed value.
    pub value: T,
    /// The source range of `value`.
    pub span: Span,
}

impl<T> Spanned<T> {
    /// Pair a value with its span.
    pub fn new(value: T, span: Span) -> Self {
        Self { value, span }
    }
}

/// The root of a parsed `Forkfile`: its statements, in source order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkFile {
    /// Top-level statements, evaluated top to bottom.
    pub statements: Vec<Stmt>,
}

/// One top-level statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stmt {
    /// `project <slug>`
    Project(Spanned<String>),
    /// `features { ... }`
    Features {
        /// Declared features and their transitive implications.
        definitions: Vec<FeatureDef>,
        /// The `features` block range.
        span: Span,
    },
    /// `postgres <cluster> { ... }`
    Postgres(PgClusterDef),
    /// `redis <cluster>`
    Redis(RedisClusterDef),
    /// `s3 <bucket>`
    S3(S3Def),
    /// `domain <id>`
    Domain(DomainDef),
    /// `env <group> { ... }`
    EnvGroup(EnvGroup),
    /// `service <id> <source> { ... }`
    Service(ServiceDef),
    /// `task <name> in <service>: ...`
    Task(TaskDef),
    /// `on <expr> { ... }` / `on <expr>: <stmt>`
    On(OnBlock),
}

impl Stmt {
    /// The span of this statement.
    pub fn span(&self) -> Span {
        match self {
            Self::Project(value) => value.span,
            Self::Features { span, .. } => *span,
            Self::Postgres(definition) => definition.span,
            Self::Redis(definition) => definition.span,
            Self::S3(definition) => definition.span,
            Self::Domain(definition) => definition.span,
            Self::EnvGroup(definition) => definition.span,
            Self::Service(definition) => definition.span,
            Self::Task(definition) => definition.span,
            Self::On(definition) => definition.span,
        }
    }
}

/// A feature-conditional block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnBlock {
    /// The condition gating every statement in `body`.
    pub cond: Condition,
    /// Statements executed when the condition holds.
    pub body: Vec<Stmt>,
    /// The whole `on ...` range.
    pub span: Span,
}

/// One declared feature and the features it enables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureDef {
    /// The feature identifier.
    pub name: Spanned<String>,
    /// Features implied by enabling this one.
    pub enables: Vec<Spanned<String>>,
    /// The whole declaration range.
    pub span: Span,
}

/// A `postgres` cluster declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PgClusterDef {
    /// The cluster identifier.
    pub id: Spanned<String>,
    /// The PostgreSQL major version (`"18"` when absent).
    pub version: Option<Spanned<String>>,
    /// Databases declared on the cluster header.
    pub databases: Vec<PgDatabaseDef>,
    /// Conditional overrides evaluated in source order.
    pub on: Vec<PgOn>,
    /// The whole declaration range.
    pub span: Span,
}

/// One `database <id>` inside a cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PgDatabaseDef {
    /// The database identifier.
    pub id: Spanned<String>,
    /// `pool = true|false` (`true` when absent).
    pub pool: Option<Spanned<bool>>,
    /// The whole declaration range.
    pub span: Span,
}

/// A conditional block inside a `postgres` cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PgOn {
    /// The gating condition.
    pub cond: Condition,
    /// Conditional version override.
    pub version: Option<Spanned<String>>,
    /// Conditional database declarations.
    pub databases: Vec<PgDatabaseDef>,
    /// The whole block range.
    pub span: Span,
}

/// A `redis` cluster declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedisClusterDef {
    /// The cluster identifier.
    pub id: Spanned<String>,
    /// The Redis major version (`"7"` when absent).
    pub version: Option<Spanned<String>>,
    /// Conditional version overrides.
    pub on: Vec<RedisOn>,
    /// The whole declaration range.
    pub span: Span,
}

/// A conditional block inside a `redis` cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedisOn {
    /// The gating condition.
    pub cond: Condition,
    /// Conditional version override.
    pub version: Option<Spanned<String>>,
    /// The whole block range.
    pub span: Span,
}

/// An `s3 <bucket>` declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct S3Def {
    /// The bucket identifier.
    pub id: Spanned<String>,
    /// The whole declaration range.
    pub span: Span,
}

/// A `domain <id>` declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainDef {
    /// The domain identifier.
    pub id: Spanned<String>,
    /// The whole declaration range.
    pub span: Span,
}

/// A reusable `env <group> { ... }` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvGroup {
    /// The group name imported through `use env <group>`.
    pub name: Spanned<String>,
    /// Variables in declaration order.
    pub vars: Vec<EnvAssignment>,
    /// The whole block range.
    pub span: Span,
}

/// One `KEY = <expr>` environment assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvAssignment {
    /// The variable name.
    pub key: Spanned<String>,
    /// The value expression.
    pub expr: Expr,
    /// The whole assignment range.
    pub span: Span,
}

/// A `service <id> ...` declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceDef {
    /// The service identifier.
    pub id: Spanned<String>,
    /// Where the service root filesystem comes from.
    pub source: ImageSource,
    /// Directives in source order (conditional blocks keep nesting).
    pub directives: Vec<ServiceDirective>,
    /// The whole declaration range.
    pub span: Span,
}

/// The container source of a service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageSource {
    /// `service <id> ./dir`: build from `<context_dir>/Dockerfile`.
    Dockerfile {
        /// The build context relative to the worktree root.
        context_dir: String,
        /// The context token range.
        span: Span,
    },
    /// `service <id> from <registry/repo:tag>`: pull an OCI image.
    OciImage {
        /// The image reference.
        image_ref: Spanned<String>,
    },
}

/// One directive inside a `service` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceDirective {
    /// `stage <name>`
    Stage(Spanned<String>),
    /// `dockerfile <relpath>`
    Dockerfile(Spanned<String>),
    /// `size "<hint>"`
    Size(Spanned<String>),
    /// `port <u16>`
    Port(Spanned<u32>),
    /// `internal_ports [<u16>, ...]`
    InternalPorts {
        /// The declared ports.
        ports: Vec<Spanned<u32>>,
        /// The whole directive range.
        span: Span,
    },
    /// `route <prefix | sub.*>`
    Route(Spanned<String>),
    /// `volume <rel_path>`
    Volume(Spanned<String>),
    /// `use env <group>`
    UseEnv(Spanned<String>),
    /// `env { KEY = <expr> }`
    EnvBlock {
        /// Variables in declaration order.
        vars: Vec<EnvAssignment>,
        /// The whole block range.
        span: Span,
    },
    /// `env.KEY = <expr>`
    EnvAssignment(EnvAssignment),
    /// `build_env { KEY = <expr> }`
    BuildEnv {
        /// Variables in declaration order.
        vars: Vec<EnvAssignment>,
        /// The whole block range.
        span: Span,
    },
    /// `run <argv...>`
    Run(Argv),
    /// `setup` / `pre-deploy` / `post-deploy` / `failed-deploy`
    Hook {
        /// The lifecycle phase.
        phase: HookPhase,
        /// One entry per command line.
        commands: Vec<Argv>,
        /// The whole directive range.
        span: Span,
    },
    /// A conditional directive block.
    On {
        /// The gating condition.
        cond: Condition,
        /// Directives applied when the condition holds.
        body: Vec<ServiceDirective>,
        /// The whole block range.
        span: Span,
    },
}

impl HookPhase {
    /// The canonical directive spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::PreDeploy => "pre-deploy",
            Self::PostDeploy => "post-deploy",
            Self::FailedDeploy => "failed-deploy",
        }
    }
}

/// A `task <name> in <service>` declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDef {
    /// The task name used by `slipfork run <name>`.
    pub name: Spanned<String>,
    /// The service the task executes inside.
    pub service: Spanned<String>,
    /// One entry per command line.
    pub commands: Vec<Argv>,
    /// `true` when the task was declared as a multi-line `{ ... }` block.
    pub multi: bool,
    /// The whole declaration range.
    pub span: Span,
}

/// A value expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Expr {
    /// A quoted string literal.
    Str(Spanned<String>),
    /// An integer literal.
    Int(Spanned<i64>),
    /// A boolean literal.
    Bool(Spanned<bool>),
    /// A dotted `<entity>.<prop>` / `app.*` / `deployment.*` reference.
    Ref(Spanned<Vec<String>>),
    /// `var(<KEY>)`
    Var(Spanned<String>),
    /// `secret(<KEY>)`
    Secret(Spanned<String>),
    /// `lhs ?? rhs`
    Fallback {
        /// The preferred expression.
        lhs: Box<Expr>,
        /// The fallback used when `lhs` has no value.
        rhs: Box<Expr>,
        /// The whole expression range.
        span: Span,
    },
    /// `a + b + ...`
    Concat {
        /// The concatenated parts.
        parts: Vec<Expr>,
        /// The whole expression range.
        span: Span,
    },
    /// A write-once CSPRNG generator.
    Generator {
        /// Which generator to run.
        kind: GeneratorKind,
        /// Its numeric argument (bytes, length, or `0` for `uuid4()`).
        arg: Spanned<u32>,
        /// The whole call range.
        span: Span,
    },
}

impl Expr {
    /// The span of this expression.
    pub fn span(&self) -> Span {
        match self {
            Self::Str(value) => value.span,
            Self::Int(value) => value.span,
            Self::Bool(value) => value.span,
            Self::Ref(value) => value.span,
            Self::Var(value) => value.span,
            Self::Secret(value) => value.span,
            Self::Fallback { span, .. }
            | Self::Concat { span, .. }
            | Self::Generator { span, .. } => *span,
        }
    }

    /// The canonical `Forkfile` source text for this expression.
    ///
    /// A claimed expression crosses OSPP verbatim, so this is the one spelling both the reference
    /// server and the daemon use for an unresolved assignment.
    pub fn render(&self) -> String {
        match self {
            Self::Str(value) => quote_text(&value.value),
            Self::Int(value) => value.value.to_string(),
            Self::Bool(value) => {
                if value.value {
                    "true".to_owned()
                } else {
                    "false".to_owned()
                }
            }
            Self::Ref(path) => path.value.join("."),
            Self::Var(key) => format!("var({})", key.value),
            Self::Secret(key) => format!("secret({})", key.value),
            Self::Fallback { lhs, rhs, .. } => format!("{} ?? {}", lhs.render(), rhs.render()),
            Self::Concat { parts, .. } => parts
                .iter()
                .map(Self::render)
                .collect::<Vec<_>>()
                .join(" + "),
            Self::Generator { kind, arg, .. } => match kind {
                GeneratorKind::Uuid4 => "uuid4()".to_owned(),
                other => format!("{}({})", other.as_str(), arg.value),
            },
        }
    }
}

fn quote_text(value: &str) -> String {
    let mut quoted = String::from("\"");
    for character in value.chars() {
        match character {
            '"' | '\\' => {
                quoted.push('\\');
                quoted.push(character);
            }
            '\n' => quoted.push_str("\\n"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// A cryptographic generator usable only as the fallback of `secret(<KEY>) ?? <generator>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeneratorKind {
    /// `random_hex(<bytes>)`
    RandomHex,
    /// `random_base64(<bytes>)`
    RandomBase64,
    /// `random_string(<len>)`
    RandomString,
    /// `uuid4()`
    Uuid4,
}

impl GeneratorKind {
    /// The canonical call spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RandomHex => "random_hex",
            Self::RandomBase64 => "random_base64",
            Self::RandomString => "random_string",
            Self::Uuid4 => "uuid4",
        }
    }
}
