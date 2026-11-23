//! Canonical string/expression rendering.

use crate::expr::*;

pub(super) fn render_expr(expr: &Expr) -> String {
    render_expr_at(expr, 0)
}

fn render_expr_at(expr: &Expr, minimum: u8) -> String {
    let precedence = expr_precedence(&expr.kind);
    let rendered = match &expr.kind {
        ExprKind::Bool(value) => value.to_string(),
        ExprKind::Int(value) => value.to_string(),
        ExprKind::String(value) => quote(value),
        ExprKind::Interpolated(parts) => render_interpolated(parts),
        ExprKind::Feature(name) => name.clone(),
        ExprKind::Var(key) => format!("var({key})"),
        ExprKind::GeneratedVar { key, generator } => {
            format!("var({key}, {})", generator.call())
        }
        ExprKind::Secret(key) => format!("secret({key})"),
        ExprKind::GeneratedSecret { key, generator } => {
            format!("secret({key}, {})", generator.call())
        }
        ExprKind::Random(generator) => generator.call(),
        ExprKind::Concat(left, right) => {
            format!("{} + {}", render_expr_at(left, 4), render_expr_at(right, 5))
        }
        ExprKind::Property { base, prop } => format!("{base}.{prop}"),
        ExprKind::Port => "$port".to_string(),
        ExprKind::EnvVar(key) => format!("$env.{key}"),
        ExprKind::Not(inner) => format!("!{}", render_expr_at(inner, 4)),
        ExprKind::And(left, right) => {
            format!("{} & {}", render_expr_at(left, 2), render_expr_at(right, 3))
        }
        ExprKind::Or(left, right) => {
            format!("{} | {}", render_expr_at(left, 1), render_expr_at(right, 2))
        }
        ExprKind::Fallback(left, right) => {
            format!(
                "{} ?? {}",
                render_expr_at(left, 0),
                render_expr_at(right, 1)
            )
        }
    };
    if precedence < minimum {
        format!("({rendered})")
    } else {
        rendered
    }
}

fn render_interpolated(parts: &[InterpPart]) -> String {
    let mut out = String::from("\"");
    for part in parts {
        match part {
            InterpPart::Text(text) => escape_text(text, &mut out),
            InterpPart::Expr(expr) => {
                out.push_str("${");
                out.push_str(&render_expr_at(expr, 0));
                out.push('}');
            }
        }
    }
    out.push('"');
    out
}

/// Escapes text for a double-quoted string; `$` is always escaped so a literal `${` can never
/// re-parse as an interpolation site.
fn escape_text(text: &str, out: &mut String) {
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '$' => out.push_str("\\$"),
            other => out.push(other),
        }
    }
}

fn expr_precedence(kind: &ExprKind) -> u8 {
    match kind {
        ExprKind::Fallback(..) => 0,
        ExprKind::Or(..) => 1,
        ExprKind::And(..) => 2,
        ExprKind::Not(_) => 3,
        _ => 5,
    }
}

pub(super) fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '$' if characters.peek() == Some(&'{') => out.push_str("\\$"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}
