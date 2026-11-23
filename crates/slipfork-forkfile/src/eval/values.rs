//! Expression and typed resource-property evaluation.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{BucketPolicy, secret_property};
use crate::check::SymbolKind;
use crate::expr::{Expr, ExprKind, GeneratorSpec, InterpPart};

/// Symbol metadata needed to evaluate `<base>.<prop>` inside one world.
#[derive(Debug, Clone)]
pub(super) enum BoundSymbol {
    Postgres {
        first_db: String,
    },
    Database {
        name: String,
    },
    Redis,
    S3 {
        bucket: String,
        policy: BucketPolicy,
    },
    Cdn {
        bucket: String,
    },
    Domain {
        id: String,
        primary: bool,
    },
}

impl BoundSymbol {
    fn is_secret_property(&self, property: &str) -> bool {
        match self {
            Self::S3 { policy, .. } => secret_property(SymbolKind::S3, *policy, property),
            Self::Postgres { .. } | Self::Database { .. } => {
                secret_property(SymbolKind::Postgres, BucketPolicy::Private, property)
            }
            Self::Redis | Self::Cdn { .. } | Self::Domain { .. } => false,
        }
    }
}

/// A secret resolver: `(key, generator)`, already bound to one component.
pub(super) type SecretLookup<'a> = dyn Fn(&str, Option<&GeneratorSpec>) -> Option<String> + 'a;

/// A variable resolver: `(key, generator)`.
pub(super) type VarLookup<'a> = dyn Fn(&str, Option<&GeneratorSpec>) -> Option<String> + 'a;

pub(super) fn is_tainted(expr: &Expr, symbols: &BTreeMap<String, BoundSymbol>) -> bool {
    match &expr.kind {
        ExprKind::Secret(_) | ExprKind::GeneratedSecret { .. } | ExprKind::Random(_) => true,
        ExprKind::Interpolated(parts) => parts.iter().any(|part| match part {
            InterpPart::Expr(inner) => is_tainted(inner, symbols),
            InterpPart::Text(_) => false,
        }),
        ExprKind::Property { base, prop } => symbols
            .get(base)
            .is_some_and(|symbol| symbol.is_secret_property(prop)),
        ExprKind::Fallback(left, right) | ExprKind::Concat(left, right) => {
            is_tainted(left, symbols) || is_tainted(right, symbols)
        }
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn eval_expr(
    expr: &Expr,
    world: &str,
    scheme: &str,
    symbols: &BTreeMap<String, BoundSymbol>,
    secret_lookup: &SecretLookup<'_>,
    var_lookup: &VarLookup<'_>,
) -> Option<String> {
    match &expr.kind {
        ExprKind::String(text) => Some(text.clone()),
        ExprKind::Interpolated(parts) => {
            let mut out = String::new();
            for part in parts {
                match part {
                    InterpPart::Text(text) => out.push_str(text),
                    InterpPart::Expr(inner) => out.push_str(&eval_expr(
                        inner,
                        world,
                        scheme,
                        symbols,
                        secret_lookup,
                        var_lookup,
                    )?),
                }
            }
            Some(out)
        }
        ExprKind::Bool(value) => Some(value.to_string()),
        ExprKind::Int(value) => Some(value.to_string()),
        ExprKind::Concat(left, right) => {
            let l = eval_expr(left, world, scheme, symbols, secret_lookup, var_lookup)?;
            let r = eval_expr(right, world, scheme, symbols, secret_lookup, var_lookup)?;
            Some(format!("{l}{r}"))
        }
        ExprKind::Fallback(left, right) => {
            eval_expr(left, world, scheme, symbols, secret_lookup, var_lookup)
                .or_else(|| eval_expr(right, world, scheme, symbols, secret_lookup, var_lookup))
        }
        ExprKind::Var(name) => var_lookup(name, None),
        ExprKind::GeneratedVar {
            key: name,
            generator,
        } => var_lookup(name, Some(generator)),
        ExprKind::Secret(secret_name) => secret_lookup(secret_name, None),
        ExprKind::GeneratedSecret {
            key: secret_name,
            generator,
        } => secret_lookup(secret_name, Some(generator)),
        ExprKind::Random(_) => {
            unreachable!("a standalone generator is rejected by the static checker")
        }
        ExprKind::Property { base, prop } if base == "self" => self_property(prop, world, scheme),
        ExprKind::Property { base, prop } => eval_property(symbols.get(base)?, prop, world),
        _ => None,
    }
}

fn self_property(prop: &str, world: &str, scheme: &str) -> Option<String> {
    let domain = format!("{world}.localhost");
    match prop {
        "domain" => Some(domain),
        "url" => Some(format!("{scheme}://{domain}")),
        "scheme" => Some(scheme.to_owned()),
        _ => None,
    }
}

fn eval_property(symbol: &BoundSymbol, prop: &str, world: &str) -> Option<String> {
    match symbol {
        BoundSymbol::Postgres { first_db } => pg_prop(first_db, prop),
        BoundSymbol::Database { name } => pg_prop(name, prop),
        BoundSymbol::Redis => match prop {
            "url" => Some("redis://127.0.0.1:6379/0".to_owned()),
            "host" => Some("127.0.0.1".to_owned()),
            "port" => Some("6379".to_owned()),
            _ => None,
        },
        BoundSymbol::S3 { bucket, .. } => match prop {
            "url" => Some(format!("http://{bucket}.{world}.localhost")),
            "endpoint" => Some("http://127.0.0.1:8333".to_owned()),
            "bucket" => Some(bucket.clone()),
            "access_key" => Some("slipfork".to_owned()),
            "secret_key" => Some(format!("slipfork-{world}-s3")),
            _ => None,
        },
        BoundSymbol::Cdn { bucket } => match prop {
            "domain" => Some(format!("{bucket}.{world}.localhost")),
            "url" => Some(format!("http://{bucket}.{world}.localhost")),
            _ => None,
        },
        BoundSymbol::Domain { id, primary } => {
            let host = if *primary {
                format!("{world}.localhost")
            } else {
                format!("{id}.{world}.localhost")
            };
            match prop {
                "name" => Some(host),
                _ => None,
            }
        }
    }
}

fn pg_prop(db: &str, prop: &str) -> Option<String> {
    match prop {
        "url" => Some(format!("postgresql://postgres@127.0.0.1:5432/{db}")),
        "host" => Some("127.0.0.1".to_owned()),
        "port" => Some("5432".to_owned()),
        "name" => Some(db.to_owned()),
        "user" => Some("postgres".to_owned()),
        "password" => Some(String::new()),
        _ => None,
    }
}

pub(super) fn expand_feature(
    name: &str,
    groups: &BTreeMap<String, Vec<String>>,
    active: &mut BTreeSet<String>,
) {
    if !active.insert(name.to_owned()) {
        return;
    }
    if let Some(children) = groups.get(name) {
        for child in children {
            expand_feature(child, groups, active);
        }
    }
}
