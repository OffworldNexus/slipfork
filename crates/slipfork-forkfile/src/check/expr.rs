//! Expression typing: feature resolution, property lookup, and secret taint.

use std::collections::BTreeSet;

use super::SymbolTable;
use crate::ast::secret_property;
use crate::diag::{Diagnostic, Diagnostics};
use crate::expr::{Expr, ExprKind, InterpPart};
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
        ExprKind::Secret(_) | ExprKind::GeneratedSecret { .. } => {
            if mode == ValueMode::Build {
                reject_secret(expr.span, diags);
            }
            true
        }
        ExprKind::Interpolated(parts) => {
            let mut tainted = false;
            for part in parts {
                if let InterpPart::Expr(inner) = part {
                    tainted |= check_value(inner, mode, symbols, diags);
                }
            }
            tainted
        }
        ExprKind::Random(generator) => {
            diags.push(Diagnostic::new(
                expr.span,
                format!(
                    "`{}` is a secret generator, not a value",
                    generator.kind.call_name()
                ),
                "pass the generator as the second argument: secret(KEY, random_string(50))",
            ));
            false
        }
        ExprKind::Property { base, prop } => {
            let tainted = property_taint(base, prop, expr.span, symbols, diags);
            if tainted && mode == ValueMode::Build {
                reject_secret(expr.span, diags);
            }
            tainted
        }
        ExprKind::Var(_)
        | ExprKind::GeneratedVar { .. }
        | ExprKind::String(_)
        | ExprKind::Int(_) => false,
        ExprKind::Bool(_) | ExprKind::Feature(_) => false,
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
            if let Some(call) = first_call(right) {
                diags.push(Diagnostic::new(
                    call.span,
                    "function calls are not allowed as `??` defaults",
                    "give the fallback a literal, `$port`, `$env.KEY`, or `self.<prop>`",
                ));
            }
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
    if base == "self" {
        if matches!(prop, "url" | "domain" | "scheme") {
            return false;
        }
        diags.push(Diagnostic::new(
            span,
            format!("`self` has no property `{prop}`"),
            "use `self.url`, `self.domain`, or `self.scheme`",
        ));
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
    if symbol.kind == super::SymbolKind::Domain && prop != "name" {
        diags.push(Diagnostic::new(
            span,
            format!("`{base}.{prop}` is not a domain property"),
            format!(
                "a domain is a bare hostname without a scheme; use '{base}.name' for the hostname, 'self.url' for the app's primary URL, or '\"${{self.scheme}}://${{{base}.name}}\"' for an alternate domain's URL"
            ),
        ));
        return false;
    }
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
    secret_property(symbol.kind, symbol.policy, prop)
}

/// The first function call in `expr`, if any; interpolation sites are walked too.
fn first_call(expr: &Expr) -> Option<&Expr> {
    match &expr.kind {
        ExprKind::Var(_)
        | ExprKind::GeneratedVar { .. }
        | ExprKind::Secret(_)
        | ExprKind::GeneratedSecret { .. }
        | ExprKind::Random(_) => Some(expr),
        ExprKind::Interpolated(parts) => parts.iter().find_map(|part| match part {
            InterpPart::Expr(inner) => first_call(inner),
            InterpPart::Text(_) => None,
        }),
        ExprKind::Not(inner) => first_call(inner),
        ExprKind::And(left, right)
        | ExprKind::Or(left, right)
        | ExprKind::Concat(left, right)
        | ExprKind::Fallback(left, right) => first_call(left).or_else(|| first_call(right)),
        _ => None,
    }
}

/// The one canonical `??`-default predicate: text that starts a function call can never be a
/// fallback value, so the migration path and the checker agree.
pub(crate) fn default_is_function_call(text: &str) -> bool {
    let text = text.trim();
    let Some((name, rest)) = text.split_once('(') else {
        return false;
    };
    !name.is_empty() && !name.contains(char::is_whitespace) && rest.ends_with(')')
}
