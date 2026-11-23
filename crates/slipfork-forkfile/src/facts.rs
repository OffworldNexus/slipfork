//! Canonical grammar facts shared by the compiler and every editor integration.
//!
//! Editors must not re-declare the language: completion lists, hover facts and
//! keyword tables come from here so the LSP, Vim, Tree-sitter and docs can never
//! drift from what the parser and checker accept.

/// Keywords the `Forkfile` language accepts.
const KEYWORDS: &[&str] = &[
    "features",
    "default",
    "image",
    "from",
    "dockerfile",
    "stage",
    "workdir",
    "arg",
    "use",
    "env",
    "service",
    "port",
    "internal_ports",
    "route",
    "strip-prefix",
    "rewrite",
    "exact",
    "domain",
    "postgres",
    "database",
    "version",
    "redis",
    "s3",
    "cdn",
    "policy",
    "on",
    "else",
    "before-start",
    "pre-deploy",
    "post-deploy",
    "failed-deploy",
    "run",
    "when",
    "changed",
];

/// Properties the reserved `self` symbol exposes: the app's own address.
const SELF_PROPERTIES: &[&str] = &["url", "domain", "scheme"];

/// Every keyword the language accepts, for completion and syntax highlighting.
#[must_use]
pub fn keywords() -> &'static [&'static str] {
    KEYWORDS
}

/// The properties `self.<property>` resolves.
#[must_use]
pub fn self_properties() -> &'static [&'static str] {
    SELF_PROPERTIES
}
