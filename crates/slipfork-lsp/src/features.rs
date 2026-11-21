//! Definition, hover, and formatting responses.

use serde_json::{Value, json};
use slipfork_forkfile::SymbolKind;
use slipfork_forkfile::boot::PrequelTool;

use crate::document::Analysis;
use crate::references::PropertySite;
use crate::text::{Position, Range, TextDocument};

/// Resolves go-to-definition at `position`.
#[must_use]
pub fn definition(
    uri: &str,
    analysis: &Analysis,
    text: &TextDocument,
    position: Position,
) -> Value {
    if let Some(reference) = analysis
        .references
        .iter()
        .find(|reference| reference.range.contains(position))
        && let Some(symbol) = analysis.symbols.get(&reference.name)
    {
        return json!([location(uri, text, symbol.span)]);
    }
    if let Some(reference) = analysis
        .env_refs
        .iter()
        .find(|reference| reference.range.contains(position))
        && let Some(span) = analysis
            .env_keys
            .get(&reference.key)
            .or_else(|| analysis.arg_keys.get(&reference.key))
    {
        return json!([location(uri, text, *span)]);
    }
    Value::Null
}

/// Resolves hover at `position`.
#[must_use]
pub fn hover(analysis: &Analysis, position: Position) -> Value {
    if let Some(tool) = analysis
        .tools
        .iter()
        .find(|tool| tool.range.contains(position))
    {
        return json!({
            "contents": { "kind": "markdown", "value": tool_hover(&tool.name) },
            "range": tool.range.to_value(),
        });
    }
    if let Some(site) = analysis
        .properties
        .iter()
        .find(|site| site.range.contains(position))
    {
        return json!({
            "contents": { "kind": "markdown", "value": property_hover(analysis, site) },
            "range": site.range.to_value(),
        });
    }
    if let Some(site) = analysis
        .env_refs
        .iter()
        .find(|site| site.range.contains(position))
    {
        return json!({
            "contents": {
                "kind": "markdown",
                "value": format!("**`$env.{}`** · runtime environment variable", site.key),
            },
            "range": site.range.to_value(),
        });
    }
    Value::Null
}

/// Returns the whole-document formatting edit, or `[]` when already canonical.
#[must_use]
pub fn formatting(analysis: &Analysis, text: &TextDocument) -> Value {
    let Some(document) = &analysis.document else {
        return Value::Null;
    };
    let formatted = slipfork_forkfile::format(document);
    if formatted == text.text() {
        return json!([]);
    }
    json!([{
        "range": Range::new(Position::new(0, 0), text.end_position()).to_value(),
        "newText": formatted,
    }])
}

fn location(uri: &str, text: &TextDocument, span: slipfork_forkfile::Span) -> Value {
    json!({ "uri": uri, "range": text.range(span).to_value() })
}

fn tool_hover(name: &str) -> String {
    match PrequelTool::lookup(name) {
        Some(tool) => format!(
            "**`@{}`** installs dependencies before this command runs.\n\nInstall: `{}`\n\nWorkspace lockfile: `{}`",
            tool.name, tool.install, tool.lockfile
        ),
        None => format!(
            "**`@{}`** is not an auto-prequel tool.\n\nUse `@uv`, `@poetry`, `@pnpm`, `@npm`, `@yarn`, `@bun`, `@composer`, `@bundle`, `@go`, or `@cargo`.",
            name
        ),
    }
}

fn property_hover(analysis: &Analysis, site: &PropertySite) -> String {
    let kind = analysis.symbols.get(&site.base).map(|symbol| symbol.kind);
    let label = kind.map_or("resource", SymbolKind::label);
    let value_type = kind
        .and_then(|kind| kind.property_type(&site.property))
        .unwrap_or("string");
    let visibility = if kind.is_some_and(|kind| kind.is_secret_property(&site.property)) {
        "Secret-tainted — redacted in logs and CLI output"
    } else {
        "Non-secret"
    };
    format!(
        "**`{}.{}`** · `{}` property\n\nType: `{}`\n\n{}",
        site.base, site.property, label, value_type, visibility
    )
}
