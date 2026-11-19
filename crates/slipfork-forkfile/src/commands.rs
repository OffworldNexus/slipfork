//! Command-line and lifecycle AST nodes.

use serde::{Deserialize, Serialize};

use crate::diag::Span;
use crate::syntax::Spanned;

/// A direct-`execve` lifecycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum HookPhase {
    /// `setup`
    Setup,
    /// `pre-deploy`
    PreDeploy,
    /// `post-deploy`
    PostDeploy,
    /// `failed-deploy`
    FailedDeploy,
}
/// A fully parsed command line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Argv {
    /// Leading `FOO=bar` per-command environment prefixes.
    pub assignments: Vec<EnvPrefix>,
    /// Argument slots in order.
    pub slots: Vec<ArgvSlot>,
    /// The whole command range.
    pub span: Span,
}
/// One `FOO=bar` per-command environment prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvPrefix {
    /// The variable name.
    pub key: Spanned<String>,
    /// The literal value.
    pub value: String,
}
/// One argument slot: text and `$` references that always produce exactly one argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgvSlot {
    /// The slot contents in order.
    pub parts: Vec<ArgPart>,
    /// The whole slot range.
    pub span: Span,
}
/// A piece of one argument slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArgPart {
    /// Literal text.
    Text(String),
    /// A `$` interpolation.
    Ref(ArgRef),
}
/// A `$` interpolation with the span it was written at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgRef {
    /// What the reference resolves to.
    pub kind: ArgRefKind,
    /// The reference range.
    pub span: Span,
}
/// The kind of a `$` interpolation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArgRefKind {
    /// `$port`
    Port,
    /// `$env.KEY`
    Env(String),
    /// `$<entity>.<prop>` (or an unknown `$token` reported by the checker).
    Property(Vec<String>),
}
