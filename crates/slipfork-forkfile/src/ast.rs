//! The `Forkfile` syntax tree: declarations, blocks, hooks, and routes.

use crate::expr::{Command, Expr};
use crate::span::Span;

pub use crate::resources::*;

/// A parsed `Forkfile` document.
#[derive(Debug, Clone)]
pub struct Forkfile {
    /// The validated format version from line 1 (`0.1`).
    pub version: String,
    /// The header comment span.
    pub span: Span,
    /// Top-level declarations in source order.
    pub items: Block<TopItem>,
}

ast_eq!(Forkfile { version, items });

/// An ordered list of declarations plus comments anchored to item positions.
#[derive(Debug, Clone, PartialEq)]
pub struct Block<T> {
    /// Declarations in declared order.
    pub items: Vec<T>,
    /// Comments anchored to declarations.
    pub comments: Vec<Comment>,
}

impl<T> Default for Block<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            comments: Vec::new(),
        }
    }
}

/// A preserved `#` line comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// Index of the declaration this comment belongs to.
    pub index: usize,
    /// The comment text, including its leading `#`.
    pub text: String,
    /// Whether the comment trails its declaration on the same line.
    pub trailing: bool,
}

/// A top-level declaration.
#[derive(Debug, Clone, PartialEq)]
pub enum TopItem {
    /// `features { ... }`
    Features(Features),
    /// `image <id> <dir|from <ref>> { ... }`
    Image(Image),
    /// `on <expr> { ... }` or `on <expr>: <declaration>`
    On(On<TopItem>),
    /// `domain <id>`
    Domain(Domain),
    /// `postgres <id> { ... }`
    Postgres(Postgres),
    /// `redis <id>`
    Redis(Redis),
    /// `s3 <bucket> { ... }`
    S3(S3),
    /// `env <group> { ... }`
    Env(Env),
}

/// A conditional block or inline declaration.
#[derive(Debug, Clone)]
pub struct On<I> {
    /// The boolean feature condition.
    pub cond: Expr,
    /// The guarded body.
    pub body: OnBody<I>,
    /// The `on` keyword span.
    pub span: Span,
}

impl<I: PartialEq> PartialEq for On<I> {
    fn eq(&self, other: &Self) -> bool {
        self.cond == other.cond && self.body == other.body
    }
}

/// The shape of an `on` body.
#[derive(Debug, Clone, PartialEq)]
pub enum OnBody<I> {
    /// `on <expr>: <declaration>`
    Inline(Box<I>),
    /// `on <expr> { ... }`
    Block(Block<I>),
}

/// `features { ... }`
#[derive(Debug, Clone)]
pub struct Features {
    /// Feature groups in declared order.
    pub groups: Block<FeatureGroup>,
    /// The `features` keyword span.
    pub span: Span,
}

ast_eq!(Features { groups });

/// One `features` entry: `default = [dev]` or a bare `mail` group.
#[derive(Debug, Clone)]
pub struct FeatureGroup {
    pub name: String,
    pub features: Option<Vec<String>>,
    pub span: Span,
}

ast_eq!(FeatureGroup { name, features });

/// `image <id> ...`
#[derive(Debug, Clone)]
pub struct Image {
    /// Image id.
    pub id: String,
    /// Where the id token was declared.
    pub id_span: Span,
    /// Where the rootfs comes from.
    pub source: ImageSource,
    /// Image body.
    pub body: Block<ImageItem>,
    /// The `image` keyword span.
    pub span: Span,
}

ast_eq!(Image { id, source, body });

/// The image rootfs source.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageSource {
    Dir(String),
    From(String),
}

/// A declaration inside an `image` block.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageItem {
    /// `dockerfile <path>`
    Dockerfile(String),
    /// `stage <name>`
    Stage(String),
    /// `workdir <subpath>`
    Workdir(String),
    /// `arg KEY = <expr>`
    Arg(ArgDecl),
    /// `arg { KEY = <expr> ... }`
    ArgBlock(ArgBlock),
    /// `use env <group>`
    UseEnv(String),
    /// `env { ... }`
    Env(EnvBlock),
    /// `before-start [when changed(...)]: <cmd>`
    BeforeStart(Hook),
    /// `pre-deploy: <cmd>`
    PreDeploy(Hook),
    /// `post-deploy: <cmd>`
    PostDeploy(Hook),
    /// `failed-deploy: <cmd>`
    FailedDeploy(Hook),
    /// `service <id> { ... }`
    Service(Service),
    /// `on <expr> ...`
    On(On<ImageItem>),
}

/// A build-time argument.
#[derive(Debug, Clone)]
pub struct ArgDecl {
    pub key: String,
    /// Where the argument key token was declared.
    pub key_span: Span,
    pub value: Expr,
    pub span: Span,
}

ast_eq!(ArgDecl { key, value });

/// `arg { ... }`
#[derive(Debug, Clone)]
pub struct ArgBlock {
    pub args: Block<ArgDecl>,
    pub span: Span,
}

ast_eq!(ArgBlock { args });

/// An inline `env { ... }` block.
#[derive(Debug, Clone)]
pub struct EnvBlock {
    pub entries: Block<EnvItem>,
    pub span: Span,
}

ast_eq!(EnvBlock { entries });

/// A top-level `env <group>` block.
#[derive(Debug, Clone)]
pub struct Env {
    pub group: String,
    pub entries: Block<EnvItem>,
    pub span: Span,
}

ast_eq!(Env { group, entries });

/// A declaration inside an `env` block.
#[derive(Debug, Clone, PartialEq)]
pub enum EnvItem {
    /// `KEY = <expr>`
    Entry(EnvEntry),
    /// `on <expr>: KEY = <expr>`
    On(On<EnvItem>),
}

/// One runtime environment entry.
#[derive(Debug, Clone)]
pub struct EnvEntry {
    pub key: String,
    pub value: Expr,
    pub span: Span,
}

ast_eq!(EnvEntry { key, value });

/// `service <id> { ... }`
#[derive(Debug, Clone)]
pub struct Service {
    pub id: String,
    /// Where the service id token was declared.
    pub id_span: Span,
    pub body: Block<ServiceItem>,
    pub span: Span,
}

ast_eq!(Service { id, body });

/// A declaration inside a `service` block.
#[derive(Debug, Clone, PartialEq)]
pub enum ServiceItem {
    /// `port <u16>`
    Port(u16),
    /// `internal_ports [ ... ]`
    InternalPorts(Vec<u16>),
    /// `route ...`
    Route(Route),
    /// `before-start [when changed(...)]: <cmd>`
    BeforeStart(Hook),
    /// `pre-deploy: <cmd>`
    PreDeploy(Hook),
    /// `post-deploy: <cmd>`
    PostDeploy(Hook),
    /// `failed-deploy: <cmd>`
    FailedDeploy(Hook),
    /// `run: <cmd>` or `run { ... }`
    Run(Hook),
    /// `on <expr> ...`
    On(On<ServiceItem>),
}

/// One ingress route declaration.
#[derive(Debug, Clone)]
pub struct Route {
    /// The optional leading path prefix (`/api`).
    pub path: Option<String>,
    /// Prefix handling.
    pub kind: RouteKind,
    /// Optional authority filter.
    pub domain: Option<DomainFilter>,
    /// The `route` keyword span.
    pub span: Span,
}

ast_eq!(Route { path, kind, domain });

/// How a route treats its matched path prefix.
#[derive(Debug, Clone, PartialEq)]
pub enum RouteKind {
    Preserve,
    StripPrefix,
    Rewrite(String),
    Exact(String),
}

/// A route authority filter: exactly one declared `domain <id>`.
#[derive(Debug, Clone)]
pub enum DomainFilter {
    /// Exact declared domain id.
    Domain {
        /// The declared domain id.
        name: String,
        /// The id token span in the `route` declaration.
        span: Span,
    },
}

impl PartialEq for DomainFilter {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Domain { name: left, .. }, Self::Domain { name: right, .. }) => left == right,
        }
    }
}

impl Eq for DomainFilter {}

/// A lifecycle hook.
#[derive(Debug, Clone)]
pub struct Hook {
    /// `when changed(...)` patterns, when declared.
    pub when_changed: Option<Vec<String>>,
    /// The command body.
    pub body: HookBody,
    /// The hook keyword span.
    pub span: Span,
}

ast_eq!(Hook { when_changed, body });

/// The shape of a hook body.
#[derive(Debug, Clone, PartialEq)]
pub enum HookBody {
    /// `: <cmd>`
    Inline(Command),
    /// `{ ... }`, executed fail-fast line by line.
    Block(Block<CommandItem>),
}

/// A line inside a command block: a command, or an `on <expr>` guard around one.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandItem {
    /// A plain shell command.
    Command(Command),
    /// `on <expr>: <item>` or `on <expr> { ... }`, nested to any depth.
    On(On<CommandItem>),
}
