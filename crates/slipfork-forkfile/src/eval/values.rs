//! Expression and typed resource-property evaluation plus deterministic offline generators.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine;
use sha2::{Digest, Sha256};

use crate::ast::{BucketPolicy, secret_property};
use crate::check::SymbolKind;
use crate::expr::{Expr, ExprKind, RandomKind};

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

pub(super) fn is_tainted(expr: &Expr, symbols: &BTreeMap<String, BoundSymbol>) -> bool {
    match &expr.kind {
        ExprKind::Secret(_) | ExprKind::Random { .. } => true,
        ExprKind::Property { base, prop } => symbols
            .get(base)
            .is_some_and(|symbol| symbol.is_secret_property(prop)),
        ExprKind::Fallback(left, right) | ExprKind::Concat(left, right) => {
            is_tainted(left, symbols) || is_tainted(right, symbols)
        }
        _ => false,
    }
}

pub(super) fn eval_expr(
    expr: &Expr,
    world: &str,
    key: &str,
    symbols: &BTreeMap<String, BoundSymbol>,
    secret_lookup: &dyn Fn(&str) -> Option<String>,
    var_lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    match &expr.kind {
        ExprKind::String(text) => Some(text.clone()),
        ExprKind::Bool(value) => Some(value.to_string()),
        ExprKind::Int(value) => Some(value.to_string()),
        ExprKind::Concat(left, right) => {
            let l = eval_expr(left, world, key, symbols, secret_lookup, var_lookup)?;
            let r = eval_expr(right, world, key, symbols, secret_lookup, var_lookup)?;
            Some(format!("{l}{r}"))
        }
        ExprKind::Fallback(left, right) => {
            eval_expr(left, world, key, symbols, secret_lookup, var_lookup)
                .or_else(|| eval_expr(right, world, key, symbols, secret_lookup, var_lookup))
        }
        ExprKind::Var(name) => var_lookup(name),
        ExprKind::Secret(secret_name) => secret_lookup(secret_name),
        ExprKind::Random { kind, n } => Some(generate_deterministic(*kind, *n, world, key)),
        ExprKind::Property { base, prop } => eval_property(symbols.get(base)?, prop, world),
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
                "domain" => Some(host),
                "url" => Some(format!("http://{host}")),
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

fn generate_deterministic(kind: RandomKind, n: Option<u64>, world: &str, key: &str) -> String {
    let len = usize::try_from(n.unwrap_or(32)).unwrap_or(32).clamp(1, 256);
    let mut bytes = Vec::with_capacity(len.max(32));
    let mut counter = 0_u32;
    while bytes.len() < len.max(32) {
        let mut hasher = Sha256::new();
        hasher.update(b"slipfork-local-random-v1:");
        hasher.update(world.as_bytes());
        hasher.update(b":");
        hasher.update(key.as_bytes());
        hasher.update(b":");
        hasher.update(counter.to_le_bytes());
        bytes.extend_from_slice(&hasher.finalize());
        counter += 1;
    }
    match kind {
        RandomKind::Hex => hex::encode(&bytes)[..len.min(bytes.len() * 2)].to_owned(),
        RandomKind::Base64 => base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(&bytes)
            .chars()
            .take(len)
            .collect(),
        RandomKind::Alnum => {
            const ALPHABET: &[u8] =
                b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
            bytes
                .iter()
                .take(len)
                .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
                .collect()
        }
        RandomKind::Uuid => format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-4{:01x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            bytes[0],
            bytes[1],
            bytes[2],
            bytes[3],
            bytes[4],
            bytes[5],
            bytes[6] & 0x0f,
            bytes[7],
            (bytes[8] & 0x3f) | 0x80,
            bytes[9],
            bytes[10],
            bytes[11],
            bytes[12],
            bytes[13],
            bytes[14],
            bytes[15]
        ),
    }
}
