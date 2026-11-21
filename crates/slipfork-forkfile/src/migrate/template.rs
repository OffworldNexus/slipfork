//! `${{ ... }}` template rendering into `Forkfile` expression AST nodes.

use std::collections::BTreeSet;

use crate::check::SymbolKind;
use crate::diag::Diagnostic;
use crate::expr::{Expr, ExprKind, RandomKind};
use crate::span::Span;

pub(crate) struct Template<'a> {
    pub pg_databases: &'a BTreeSet<String>,
    pub redis: &'a BTreeSet<String>,
    pub buckets: &'a BTreeSet<String>,
    pub domains: &'a BTreeSet<String>,
    pub components: &'a BTreeSet<String>,
}

pub(crate) fn render(
    raw: &str,
    context: &Template<'_>,
    path: &str,
    span: Span,
) -> Result<Expr, Diagnostic> {
    if raw.contains("!!") {
        return Err(Diagnostic::new(
            span,
            format!("{path}: `!!` tags cannot be migrated"),
            "remove the `!!` tag; only `${{ namespace::value }}` is understood",
        ));
    }
    let mut parts = Vec::new();
    let mut index = 0;
    while let Some(offset) = raw[index..].find("${{") {
        let start = index + offset;
        if start > index {
            parts.push(string(&raw[index..start], span));
        }
        let open = start + 3;
        let Some(close) = raw[open..].find("}}") else {
            return Err(Diagnostic::new(
                span,
                format!("{path}: unterminated `${{{{ ... }}}}` template"),
                "close the expression with `}}`",
            ));
        };
        let end = open + close;
        parts.push(expression(raw[open..end].trim(), context, path, span)?);
        index = end + 2;
    }
    if index < raw.len() {
        parts.push(string(&raw[index..], span));
    }
    if parts.is_empty() {
        return Ok(string(raw, span));
    }
    let mut parts = parts.into_iter();
    let first = parts.next().expect("at least one part");
    Ok(parts.fold(first, |left, right| {
        expr(ExprKind::Concat(Box::new(left), Box::new(right)), span)
    }))
}

fn expression(
    body: &str,
    context: &Template<'_>,
    path: &str,
    span: Span,
) -> Result<Expr, Diagnostic> {
    let (value, default) = match body.split_once("??") {
        Some((left, right)) => (left.trim(), Some(right.trim())),
        None => (body, None),
    };
    let value = chain(value, context, path, span)?;
    match default {
        Some(default) => {
            let default = default_expr(default, path, span)?;
            Ok(expr(
                ExprKind::Fallback(Box::new(value), Box::new(default)),
                span,
            ))
        }
        None => Ok(value),
    }
}

fn chain(value: &str, context: &Template<'_>, path: &str, span: Span) -> Result<Expr, Diagnostic> {
    let Some((namespace, rest)) = value.split_once("::") else {
        return Err(Diagnostic::new(
            span,
            format!("{path}: `{value}` is not a `${{{{ namespace::value }}}}` template"),
            "use `secrets::KEY`, `vars::KEY`, `resources::...`, `domains::<id>`, `components::<id>.<prop>`, or `deployment::<prop>`",
        ));
    };
    match namespace {
        "secrets" => Ok(expr(
            ExprKind::Secret(non_empty(rest, path, span)?.to_string()),
            span,
        )),
        "vars" => Ok(expr(
            ExprKind::Var(non_empty(rest, path, span)?.to_string()),
            span,
        )),
        "domains" => {
            let id = non_empty(rest, path, span)?;
            if !context.domains.contains(id) {
                return Err(unknown(path, "domain", id, span));
            }
            Ok(property(id, "domain", span))
        }
        "deployment" => Ok(property("deployment", non_empty(rest, path, span)?, span)),
        "components" => {
            let (id, prop) = rest.split_once('.').ok_or_else(|| {
                Diagnostic::new(
                    span,
                    format!("{path}: `components::{rest}` needs `<id>.<prop>`"),
                    "write `${{ components::api.url }}`",
                )
            })?;
            if !context.components.contains(id) {
                return Err(unknown(path, "component", id, span));
            }
            Ok(property(id, prop, span))
        }
        "resources" => resources(rest, context, path, span),
        other => Err(Diagnostic::new(
            span,
            format!("{path}: unsupported template namespace `{other}`"),
            "use only `secrets`, `vars`, `resources`, `domains`, `components`, or `deployment`",
        )),
    }
}

fn resources(
    rest: &str,
    context: &Template<'_>,
    path: &str,
    span: Span,
) -> Result<Expr, Diagnostic> {
    if let Some(prop) = rest.strip_prefix("app.") {
        return Ok(property("app", non_empty(prop, path, span)?, span));
    }
    let mut parts = rest.splitn(3, '.');
    let family = parts.next().unwrap_or_default();
    let id = parts.next().unwrap_or_default();
    let prop = parts.next().unwrap_or_default();
    if id.is_empty() || prop.is_empty() {
        return Err(Diagnostic::new(
            span,
            format!("{path}: `resources::{rest}` needs `<family>.<id>.<prop>`"),
            "write `${{ resources::pg_databases.app_db.url }}`",
        ));
    }
    let (known, kind) = match family {
        "pg_databases" => (context.pg_databases, SymbolKind::Database),
        "redis_clusters" => (context.redis, SymbolKind::Redis),
        "s3_buckets" => (context.buckets, SymbolKind::S3),
        other => {
            return Err(Diagnostic::new(
                span,
                format!("{path}: unsupported resource family `{other}`"),
                "use `pg_databases`, `redis_clusters`, `s3_buckets`, or `app`",
            ));
        }
    };
    if !known.contains(id) {
        return Err(unknown(path, family, id, span));
    }
    if let Some(properties) = kind.properties()
        && !properties.contains(&prop)
    {
        return Err(Diagnostic::new(
            span,
            format!("{path}: `{id}.{prop}` is not a typed property"),
            format!("use one of `{id}.{}`", properties.join("`, `")),
        ));
    }
    Ok(property(id, prop, span))
}

fn default_expr(text: &str, path: &str, span: Span) -> Result<Expr, Diagnostic> {
    if text.is_empty() {
        return Err(Diagnostic::new(
            span,
            format!("{path}: empty `??` default"),
            "add a fallback value after `??`",
        ));
    }
    if let Some(inner) = text
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    {
        return Ok(string(inner, span));
    }
    if let Ok(number) = text.parse::<u64>() {
        return Ok(expr(ExprKind::Int(number), span));
    }
    match text {
        "true" => return Ok(expr(ExprKind::Bool(true), span)),
        "false" => return Ok(expr(ExprKind::Bool(false), span)),
        _ => {}
    }
    if let Some(key) = call(text, "var") {
        return Ok(expr(ExprKind::Var(unquote(key)), span));
    }
    if let Some(key) = call(text, "secret") {
        return Ok(expr(ExprKind::Secret(unquote(key)), span));
    }
    if call(text, "random_uuid").is_some() {
        return Ok(expr(
            ExprKind::Random {
                kind: RandomKind::Uuid,
                n: None,
            },
            span,
        ));
    }
    for (name, kind) in [
        ("random_hex", RandomKind::Hex),
        ("random_base64", RandomKind::Base64),
        ("random_alnum", RandomKind::Alnum),
    ] {
        if let Some(length) = call(text, name) {
            let n = if length.is_empty() {
                None
            } else {
                length.parse::<u64>().ok()
            };
            return Ok(expr(ExprKind::Random { kind, n }, span));
        }
    }
    Ok(string(text, span))
}

fn call<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.strip_prefix(name)?
        .strip_prefix('(')?
        .strip_suffix(')')
}

fn unquote(value: &str) -> String {
    value.trim().trim_matches('"').to_string()
}

fn non_empty<'a>(value: &'a str, path: &str, span: Span) -> Result<&'a str, Diagnostic> {
    if value.is_empty() {
        Err(Diagnostic::new(
            span,
            format!("{path}: template value is empty"),
            "name a key, resource, domain, or property",
        ))
    } else {
        Ok(value)
    }
}

fn unknown(path: &str, kind: &str, id: &str, span: Span) -> Diagnostic {
    Diagnostic::new(
        span,
        format!("{path}: unknown {kind} `{id}`"),
        format!("declare `{id}` in `snow.yml` before referencing it"),
    )
}

fn expr(kind: ExprKind, span: Span) -> Expr {
    Expr { kind, span }
}

fn string(value: &str, span: Span) -> Expr {
    expr(ExprKind::String(value.to_string()), span)
}

fn property(base: &str, prop: &str, span: Span) -> Expr {
    expr(
        ExprKind::Property {
            base: base.to_string(),
            prop: prop.to_string(),
        },
        span,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context<'a>(
        pg: &'a BTreeSet<String>,
        redis: &'a BTreeSet<String>,
        buckets: &'a BTreeSet<String>,
        domains: &'a BTreeSet<String>,
        components: &'a BTreeSet<String>,
    ) -> Template<'a> {
        Template {
            pg_databases: pg,
            redis,
            buckets,
            domains,
            components,
        }
    }

    #[test]
    fn literal_text_becomes_a_concat_expression() {
        let pg: BTreeSet<String> = ["main".to_string()].into_iter().collect();
        let empty = BTreeSet::new();
        let template = context(&pg, &empty, &empty, &empty, &empty);
        let rendered = render(
            "postgres://${{ resources::pg_databases.main.url }}",
            &template,
            "envs.common.DATABASE_URL",
            Span::at(crate::Position::new(1, 1, 0)),
        )
        .expect("renders");
        let ExprKind::Concat(left, right) = rendered.kind else {
            panic!("expected concat");
        };
        assert!(matches!(&left.kind, ExprKind::String(text) if text == "postgres://"));
        assert!(matches!(
            &right.kind,
            ExprKind::Property { base, prop } if base == "main" && prop == "url"
        ));
    }

    #[test]
    fn unknown_namespace_is_refused_with_its_path() {
        let empty = BTreeSet::new();
        let template = context(&empty, &empty, &empty, &empty, &empty);
        let error = render(
            "${{ rollback::x }}",
            &template,
            "envs.common.KEY",
            Span::at(crate::Position::new(2, 3, 0)),
        )
        .expect_err("refused");
        assert!(error.message.contains("envs.common.KEY"));
        assert!(error.message.contains("rollback"));
    }

    #[test]
    fn fallback_defaults_stay_typed() {
        let empty = BTreeSet::new();
        let template = context(&empty, &empty, &empty, &empty, &empty);
        let rendered = render(
            "${{ secrets::SECRET_KEY ?? random_hex(32) }}",
            &template,
            "envs.common.SECRET_KEY",
            Span::at(crate::Position::new(1, 1, 0)),
        )
        .expect("renders");
        let ExprKind::Fallback(left, right) = rendered.kind else {
            panic!("expected fallback");
        };
        assert!(matches!(&left.kind, ExprKind::Secret(key) if key == "SECRET_KEY"));
        assert!(matches!(
            right.kind,
            ExprKind::Random {
                kind: RandomKind::Hex,
                n: Some(32)
            }
        ));
    }
}
