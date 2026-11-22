//! Reference sites harvested from the AST for definition and hover.

use slipfork_forkfile::ast::*;
use slipfork_forkfile::{Command, CommandPart, Expr, ExprKind, Span};

use crate::text::{Range, TextDocument, find_word};

/// A reference to a declared symbol (property base or route authority).
#[derive(Debug, Clone)]
pub struct SymbolRef {
    /// The identifier under the cursor.
    pub range: Range,
    /// The referenced symbol id.
    pub name: String,
}

/// A routed property such as `<db>.url` or `<cdn>.domain`.
#[derive(Debug, Clone)]
pub struct PropertySite {
    /// The whole property expression.
    pub range: Range,
    /// The resource base identifier.
    pub base: String,
    /// The typed property name.
    pub property: String,
}

/// An `@<tool>` prequel site.
#[derive(Debug, Clone)]
pub struct ToolSite {
    /// The `@tool` token.
    pub range: Range,
    /// The tool name without `@`.
    pub name: String,
}

/// One `policy <private|public-read>` declaration.
#[derive(Debug, Clone)]
pub struct PolicySite {
    /// The `policy` keyword.
    pub range: Range,
    /// The declared bucket policy.
    pub policy: slipfork_forkfile::BucketPolicy,
}

/// A `$env.KEY`, `var(KEY)`, or `secret(KEY)` site.
#[derive(Debug, Clone)]
pub struct EnvRefSite {
    /// The key token.
    pub range: Range,
    /// The referenced environment key.
    pub key: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Collected {
    pub references: Vec<SymbolRef>,
    pub properties: Vec<PropertySite>,
    pub tools: Vec<ToolSite>,
    pub policies: Vec<PolicySite>,
    pub env_refs: Vec<EnvRefSite>,
}

pub(crate) fn collect(document: &Forkfile, text: &TextDocument) -> Collected {
    let mut out = Collected::default();
    for item in &document.items.items {
        top(item, text, &mut out);
    }
    out
}

fn top(item: &TopItem, text: &TextDocument, out: &mut Collected) {
    match item {
        TopItem::Features(_) | TopItem::Domain(_) => {}
        TopItem::Image(image) => {
            for item in &image.body.items {
                image_item(item, text, out);
            }
        }
        TopItem::On(on) => walk_on(&on.cond, &on.body, top, text, out),
        TopItem::Postgres(postgres) => {
            for item in &postgres.body.items {
                postgres_item(item, text, out);
            }
        }
        TopItem::Redis(redis) => {
            for item in &redis.body.items {
                redis_item(item, text, out);
            }
        }
        TopItem::S3(s3) => {
            for item in &s3.body.items {
                s3_item(item, text, out);
            }
        }
        TopItem::Env(env) => {
            for item in &env.entries.items {
                env_item(item, text, out);
            }
        }
    }
}

fn image_item(item: &ImageItem, text: &TextDocument, out: &mut Collected) {
    match item {
        ImageItem::Dockerfile(_) | ImageItem::Stage(_) | ImageItem::Workdir(_) => {}
        ImageItem::Arg(arg) => expression(&arg.value, text, out),
        ImageItem::ArgBlock(block) => {
            for arg in &block.args.items {
                expression(&arg.value, text, out);
            }
        }
        ImageItem::UseEnv(_) => {}
        ImageItem::Env(env) => {
            for item in &env.entries.items {
                env_item(item, text, out);
            }
        }
        ImageItem::BeforeStart(hook)
        | ImageItem::PreDeploy(hook)
        | ImageItem::PostDeploy(hook)
        | ImageItem::FailedDeploy(hook) => hook_body(hook, text, out),
        ImageItem::Service(service) => {
            for item in &service.body.items {
                service_item(item, text, out);
            }
        }
        ImageItem::On(on_item) => walk_on(&on_item.cond, &on_item.body, image_item, text, out),
    }
}

fn service_item(item: &ServiceItem, text: &TextDocument, out: &mut Collected) {
    match item {
        ServiceItem::Port(_) | ServiceItem::InternalPorts(_) => {}
        ServiceItem::Route(route) => route_reference(route, text, out),
        ServiceItem::BeforeStart(hook)
        | ServiceItem::PreDeploy(hook)
        | ServiceItem::PostDeploy(hook)
        | ServiceItem::FailedDeploy(hook)
        | ServiceItem::Run(hook) => hook_body(hook, text, out),
        ServiceItem::On(on_item) => {
            walk_on(&on_item.cond, &on_item.body, service_item, text, out);
        }
    }
}

fn postgres_item(item: &PostgresItem, text: &TextDocument, out: &mut Collected) {
    match item {
        PostgresItem::Database(_) | PostgresItem::Version(_) => {}
        PostgresItem::On(on_item) => {
            walk_on(&on_item.cond, &on_item.body, postgres_item, text, out);
        }
    }
}

fn redis_item(item: &RedisItem, text: &TextDocument, out: &mut Collected) {
    match item {
        RedisItem::Version(_) => {}
        RedisItem::On(on_item) => walk_on(&on_item.cond, &on_item.body, redis_item, text, out),
    }
}

fn s3_item(item: &S3Item, text: &TextDocument, out: &mut Collected) {
    match item {
        S3Item::Policy(decl) => out.policies.push(PolicySite {
            range: text.range(decl.span),
            policy: decl.policy,
        }),
        S3Item::Cdn(_) => {}
        S3Item::On(on_item) => walk_on(&on_item.cond, &on_item.body, s3_item, text, out),
    }
}

fn env_item(item: &EnvItem, text: &TextDocument, out: &mut Collected) {
    match item {
        EnvItem::Entry(entry) => expression(&entry.value, text, out),
        EnvItem::On(on_item) => walk_on(&on_item.cond, &on_item.body, env_item, text, out),
    }
}

fn walk_on<I>(
    condition: &Expr,
    body: &OnBody<I>,
    walk: fn(&I, &TextDocument, &mut Collected),
    text: &TextDocument,
    out: &mut Collected,
) {
    expression(condition, text, out);
    match body {
        OnBody::Inline(item) => walk(item, text, out),
        OnBody::Block(block) => {
            for item in &block.items {
                walk(item, text, out);
            }
        }
    }
}

fn hook_body(hook: &Hook, text: &TextDocument, out: &mut Collected) {
    match &hook.body {
        HookBody::Inline(command) => command_site(command, text, out),
        HookBody::Block(block) => {
            for item in &block.items {
                command_item(item, text, out);
            }
        }
    }
}

fn command_item(item: &CommandItem, text: &TextDocument, out: &mut Collected) {
    match item {
        CommandItem::Command(command) => command_site(command, text, out),
        CommandItem::On(on_item) => walk_on(&on_item.cond, &on_item.body, command_item, text, out),
    }
}

fn expression(expr: &Expr, text: &TextDocument, out: &mut Collected) {
    match &expr.kind {
        ExprKind::Property { base, prop } => {
            let start = expr.span.start.offset;
            out.references.push(SymbolRef {
                range: bytes_range(text, start, base.len()),
                name: base.clone(),
            });
            out.properties.push(PropertySite {
                range: text.range(expr.span),
                base: base.clone(),
                property: prop.clone(),
            });
        }
        ExprKind::Var(key) | ExprKind::Secret(key) => {
            let source = slice(text.text(), expr.span);
            if let Some(offset) = find_word(source, key) {
                let start = expr.span.start.offset + offset;
                out.env_refs.push(EnvRefSite {
                    range: bytes_range(text, start, key.len()),
                    key: key.clone(),
                });
            }
        }
        ExprKind::Not(inner) => expression(inner, text, out),
        ExprKind::And(left, right)
        | ExprKind::Or(left, right)
        | ExprKind::Concat(left, right)
        | ExprKind::Fallback(left, right) => {
            expression(left, text, out);
            expression(right, text, out);
        }
        _ => {}
    }
}

fn command_site(command: &Command, text: &TextDocument, out: &mut Collected) {
    if let Some(tool) = &command.tool {
        out.tools.push(ToolSite {
            range: text.range(tool.span),
            name: tool.name.clone(),
        });
    }
    for part in &command.parts {
        if let CommandPart::Env { key, span } = part {
            out.env_refs.push(EnvRefSite {
                range: text.range(*span),
                key: key.clone(),
            });
        }
    }
}

fn route_reference(route: &Route, text: &TextDocument, out: &mut Collected) {
    if let Some(DomainFilter::Domain { name, span }) = &route.domain {
        out.references.push(SymbolRef {
            range: text.range(*span),
            name: name.clone(),
        });
    }
}

fn slice(source: &str, span: Span) -> &str {
    source
        .get(span.start.offset..span.end.offset)
        .unwrap_or_default()
}

fn bytes_range(text: &TextDocument, start: usize, len: usize) -> Range {
    Range::new(text.position(start), text.position(start + len))
}
