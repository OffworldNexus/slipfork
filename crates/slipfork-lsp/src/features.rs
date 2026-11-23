//! Definition, hover, completion, and formatting responses.

use serde_json::{Value, json};
use slipfork_forkfile::BucketPolicy;
use slipfork_forkfile::SymbolKind;
use slipfork_forkfile::boot::PrequelTool;
use slipfork_forkfile::secret_property;

use crate::document::Analysis;
use crate::references::PropertySite;
use crate::text::{Position, Range, TextDocument};

/// LSP `CompletionItemKind::Property`.
const PROPERTY_KIND: u8 = 10;
/// LSP `CompletionItemKind::Value`.
const VALUE_KIND: u8 = 12;
/// LSP `CompletionItemKind::Keyword`.
const KEYWORD_KIND: u8 = 14;

/// Description of one reserved `self` property: the app's own address.
fn self_detail(property: &str) -> &'static str {
    match property {
        "url" => "`\"https://<world>.localhost\"` — primary URL with a scheme",
        "domain" => "`\"<world>.localhost\"` — primary bare hostname, no scheme",
        "scheme" => "`\"http\"` locally, `\"https\"` behind a paired ingress",
        _ => "`self` property",
    }
}

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
        .policies
        .iter()
        .find(|site| site.range.contains(position))
    {
        return json!({
            "contents": { "kind": "markdown", "value": policy_hover(site.policy) },
            "range": site.range.to_value(),
        });
    }
    if let Some(site) = analysis
        .generators
        .iter()
        .find(|site| site.range.contains(position))
    {
        return json!({
            "contents": { "kind": "markdown", "value": generator_hover(site.name) },
            "range": site.range.to_value(),
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

/// Offers properties after `<base>.` and keywords everywhere else.
#[must_use]
pub fn completion(analysis: &Analysis, text: &TextDocument, position: Position) -> Value {
    let offset = text.offset(position);
    let before = &text.text()[text.line_start(position.line)..offset];
    if let Some(dot) = before.rfind('.') {
        let base = before[..dot]
            .chars()
            .rev()
            .take_while(|character| {
                character.is_ascii_alphanumeric() || *character == '_' || *character == '-'
            })
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        if !base.is_empty() {
            let items = properties(analysis, &base)
                .into_iter()
                .map(|(label, detail)| {
                    json!({
                        "label": label,
                        "kind": PROPERTY_KIND,
                        "detail": detail,
                        "documentation": { "kind": "markdown", "value": detail },
                    })
                })
                .collect::<Vec<_>>();
            return json!({ "isIncomplete": false, "items": items });
        }
    }
    let mut items = slipfork_forkfile::keywords()
        .iter()
        .map(|keyword| {
            json!({ "label": keyword, "kind": KEYWORD_KIND, "detail": "Forkfile keyword" })
        })
        .collect::<Vec<_>>();
    items.push(json!({
        "label": "self",
        "kind": VALUE_KIND,
        "detail": "`self.url`, `self.domain`, `self.scheme`",
    }));
    json!({ "isIncomplete": false, "items": items })
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

fn properties(analysis: &Analysis, base: &str) -> Vec<(&'static str, &'static str)> {
    if base == "self" {
        return slipfork_forkfile::self_properties()
            .iter()
            .map(|property| (*property, self_detail(property)))
            .collect();
    }
    let Some(symbol) = analysis.symbols.get(base) else {
        return Vec::new();
    };
    symbol
        .kind
        .properties()
        .unwrap_or_default()
        .iter()
        .map(|property| (*property, "typed property"))
        .collect()
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

fn policy_hover(policy: BucketPolicy) -> String {
    match policy {
        BucketPolicy::Private => "**`policy private`** · the default: anonymous `GET`/`HEAD` answer `403`.\n\n`cdn <id>` requires `policy public-read`.".to_owned(),
        BucketPolicy::PublicRead => "**`policy public-read`** · anonymous `GET`/`HEAD` answer `200`; writes still require SigV4.\n\n`policy private` is the default; `cdn <id>` requires this policy.".to_owned(),
    }
}

fn generator_hover(name: &str) -> String {
    format!(
        "**`{name}`** · secret generator, only valid as the second argument of `secret(KEY, {name})` or `var(KEY, {name})`.\n\nA generator is never a `??` fallback: `??` takes deterministic expressions only, so write `secret(SECRET_KEY, random_string(50))` — not `secret(SECRET_KEY) ?? random_string(50)`."
    )
}

fn property_hover(analysis: &Analysis, site: &PropertySite) -> String {
    if site.base == "self" {
        if !slipfork_forkfile::self_properties().contains(&site.property.as_str()) {
            return format!(
                "**`self.{}`** · unknown\n\nUse `self.url`, `self.domain`, or `self.scheme`.",
                site.property
            );
        }
        return format!(
            "**`self.{}`** · the app's own address\n\n{}",
            site.property,
            self_detail(&site.property)
        );
    }
    let kind = analysis.symbols.get(&site.base).map(|symbol| symbol.kind);
    if kind == Some(SymbolKind::Domain) {
        return format!(
            "**`{}.{}`** · `domain` property\n\nType: `string`\n\nA domain is a bare hostname; `{}.name` is the only property. Use `self.url` for a URL with a scheme, or `\"${{self.scheme}}://${{{}.name}}\"` for this alternate domain.",
            site.base, site.property, site.base, site.base
        );
    }
    let label = kind.map_or("resource", SymbolKind::label);
    let value_type = kind
        .and_then(|kind| kind.property_type(&site.property))
        .unwrap_or("string");
    let visibility = if analysis
        .symbols
        .get(&site.base)
        .is_some_and(|symbol| secret_property(symbol.kind, symbol.policy, &site.property))
    {
        "Secret-tainted — redacted in logs and CLI output"
    } else {
        "Non-secret"
    };
    format!(
        "**`{}.{}`** · `{}` property\n\nType: `{}`\n\n{}",
        site.base, site.property, label, value_type, visibility
    )
}
