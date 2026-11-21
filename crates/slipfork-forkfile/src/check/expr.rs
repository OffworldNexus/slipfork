//! Expression typing: feature resolution, property lookup, and secret taint.

use std::collections::BTreeSet;

use super::SymbolTable;
use crate::diag::{Diagnostic, Diagnostics};
use crate::expr::{Expr, ExprKind};
use crate::span::Span;

/// Where an expression is evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ValueMode {
    /// Build-time `arg`: strictly non-secret and never runtime-only.
    Build,
    /// Runtime `env`: secrets are allowed and redacted at use sites.
    Runtime,
}

pub(super) fn check_condition(expr: &Expr, features: &BTreeSet<String>, diags: &mut Diagnostics) {
    match &expr.kind {
        ExprKind::Feature(name) => {
            if !features.contains(name) {
                diags.push(Diagnostic::new(
                    expr.span,
                    format!("unknown feature `{name}` in `on` condition"),
                    format!("declare `{name}` in `features {{ ... }}`"),
                ));
            }
        }
        ExprKind::Not(inner) => check_condition(inner, features, diags),
        ExprKind::And(left, right)
        | ExprKind::Or(left, right)
        | ExprKind::Concat(left, right)
        | ExprKind::Fallback(left, right) => {
            check_condition(left, features, diags);
            check_condition(right, features, diags);
        }
        _ => {}
    }
}

/// Types `expr`, reporting misuse and returning whether it is secret-tainted.
pub(super) fn check_value(
    expr: &Expr,
    mode: ValueMode,
    symbols: &SymbolTable,
    diags: &mut Diagnostics,
) -> bool {
    match &expr.kind {
        ExprKind::Secret(_) => {
            if mode == ValueMode::Build {
                reject_secret(expr.span, diags);
            }
            true
        }
        ExprKind::Property { base, prop } => {
            let tainted = property_taint(base, prop, expr.span, symbols, diags);
            if tainted && mode == ValueMode::Build {
                reject_secret(expr.span, diags);
            }
            tainted
        }
        ExprKind::Var(_)
        | ExprKind::Random { .. }
        | ExprKind::String(_)
        | ExprKind::Int(_)
        | ExprKind::Bool(_)
        | ExprKind::Feature(_) => false,
        ExprKind::Port | ExprKind::EnvVar(_) => {
            if mode == ValueMode::Build {
                diags.push(Diagnostic::new(
                    expr.span,
                    "`arg` is build-time but `$port` / `$env.KEY` only exist at runtime",
                    "move runtime values into `env { ... }`",
                ));
            }
            false
        }
        ExprKind::Not(inner) => check_value(inner, mode, symbols, diags),
        ExprKind::And(left, right) | ExprKind::Or(left, right) | ExprKind::Concat(left, right) => {
            let left = check_value(left, mode, symbols, diags);
            let right = check_value(right, mode, symbols, diags);
            left | right
        }
        ExprKind::Fallback(left, right) => {
            let left = check_value(left, mode, symbols, diags);
            let right = check_value(right, mode, symbols, diags);
            left | right
        }
    }
}

fn reject_secret(span: Span, diags: &mut Diagnostics) {
    diags.push(Diagnostic::new(
        span,
        "build-time `arg` would bake a secret into the image",
        "move the secret into `env { ... }`; `arg` values are visible in image layers",
    ));
}

fn property_taint(
    base: &str,
    prop: &str,
    span: Span,
    symbols: &SymbolTable,
    diags: &mut Diagnostics,
) -> bool {
    if base == "app" || base == "deployment" {
        return false;
    }
    let Some(symbol) = symbols.get(base) else {
        diags.push(Diagnostic::new(
            span,
            format!("unknown symbol `{base}`"),
            format!("declare `{base}` before referencing `{base}.{prop}`"),
        ));
        return false;
    };
    let Some(properties) = symbol.kind.properties() else {
        diags.push(Diagnostic::new(
            span,
            format!(
                "`{base}` is a declared `{}` with no typed properties",
                symbol.kind.label()
            ),
            "reference a resource property such as `app_db.url`",
        ));
        return false;
    };
    if !properties.contains(&prop) {
        diags.push(Diagnostic::new(
            span,
            format!("`{base}` has no property `{prop}`"),
            format!(
                "use `{base}.` followed by one of `{}`",
                properties.join("`, `")
            ),
        ));
        return false;
    }
    symbol.kind.is_secret_property(prop)
}
