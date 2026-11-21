//! One open document: parser output, checker diagnostics, and reference sites.

use std::collections::BTreeMap;

use slipfork_forkfile::{Diagnostic, Forkfile, Span, SymbolTable, check, parse};

use crate::references::{self, EnvRefSite, PolicySite, PropertySite, SymbolRef, ToolSite};
use crate::text::TextDocument;

/// Everything the language features need for one document version.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    /// Errors in source order.
    pub diagnostics: Vec<Diagnostic>,
    /// The parsed document, when parsing succeeded.
    pub document: Option<Forkfile>,
    /// The flat symbol table, including ids from a document that fails checking.
    pub symbols: SymbolTable,
    /// Runtime `env` keys declared by the checker.
    pub env_keys: BTreeMap<String, Span>,
    /// Build-time `arg` keys declared by the checker.
    pub arg_keys: BTreeMap<String, Span>,
    /// Property expressions.
    pub properties: Vec<PropertySite>,
    /// `@<tool>` prequels.
    pub tools: Vec<ToolSite>,
    /// `policy` declarations.
    pub policies: Vec<PolicySite>,
    /// Environment references.
    pub env_refs: Vec<EnvRefSite>,
    /// Symbol references.
    pub references: Vec<SymbolRef>,
}

impl Analysis {
    /// Parses, checks, and harvests references for `text`.
    #[must_use]
    pub fn new(text: &TextDocument) -> Self {
        let document = match parse(text.text()) {
            Ok(document) => document,
            Err(diagnostics) => {
                return Self {
                    diagnostics: diagnostics.items,
                    ..Self::default()
                };
            }
        };
        let (symbols, env_keys, arg_keys, diagnostics) = match check(&document) {
            Ok(checked) => (
                checked.symbols,
                checked.env_keys,
                checked.arg_keys,
                Vec::new(),
            ),
            Err((checked, diagnostics)) => (
                checked.symbols,
                checked.env_keys,
                checked.arg_keys,
                diagnostics.items,
            ),
        };
        let collected = references::collect(&document, text);
        Self {
            diagnostics,
            document: Some(document),
            symbols,
            env_keys,
            arg_keys,
            properties: collected.properties,
            tools: collected.tools,
            policies: collected.policies,
            env_refs: collected.env_refs,
            references: collected.references,
        }
    }
}
