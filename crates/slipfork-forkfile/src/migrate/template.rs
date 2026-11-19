//! Faceless `${{ … }}` → `Forkfile` expression translation and shell-shaped argv splitting.

use super::{MigrateError, schema};

/// Translate one Faceless `${{ ... }}` template into a `Forkfile` expression.
pub(super) fn render_template(template: &str, path: &str) -> Result<String, MigrateError> {
    let parts = template_parts(template, path)?;
    if parts.is_empty() {
        return Ok(quote_text(template));
    }
    let mut rendered = Vec::new();
    for part in parts {
        match part {
            TemplatePart::Text(text) => rendered.push(quote_text(&text)),
            TemplatePart::Expression(expression) => {
                rendered.push(translate_expression(&expression, path)?)
            }
        }
    }
    Ok(rendered.join(" + "))
}

/// A double-quoted `Forkfile` string literal.
pub(super) fn quote_text(value: &str) -> String {
    let mut quoted = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' | '\\' => {
                quoted.push('\\');
                quoted.push(ch);
            }
            '\n' => quoted.push_str("\\n"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// Split one upstream shell-shaped `run_command` into direct `argv`.
///
/// `Forkfile` runs `execve` without a shell, so any shell operator (redirection, pipes,
/// substitution, globbing) fails the migration instead of silently changing meaning.
pub(super) fn split_command(command: &str, path: &str) -> Result<Vec<String>, MigrateError> {
    let mut argv = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else if ch == '\\' && active == '"' {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            } else {
                word.push(ch);
            }
            in_word = true;
            continue;
        }
        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                in_word = true;
            }
            '\\' => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
                in_word = true;
            }
            ch if ch.is_whitespace() => {
                if in_word {
                    argv.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '|' | '&' | ';' | '<' | '>' | '$' | '`' | '*' | '?' | '(' | ')' | '{' | '}' | '['
            | ']' | '~' | '!' => {
                return Err(MigrateError::ShellSyntax {
                    path: path.to_owned(),
                    reason: format!("`{ch}` needs a shell"),
                });
            }
            other => {
                word.push(other);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err(MigrateError::ShellSyntax {
            path: path.to_owned(),
            reason: "unterminated quote".to_owned(),
        });
    }
    if in_word {
        argv.push(word);
    }
    Ok(argv)
}

pub(super) enum TemplatePart {
    Text(String),
    Expression(String),
}

/// Every `${{ ... }}` occurrence, with the literal text between them.
pub(super) fn template_parts(
    template: &str,
    path: &str,
) -> Result<Vec<TemplatePart>, MigrateError> {
    let mut parts = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("${{") {
        if start > 0 {
            parts.push(TemplatePart::Text(rest[..start].to_owned()));
        }
        let after = &rest[start + 3..];
        let Some(end) = after.find("}}") else {
            return Err(schema(
                path,
                format!("unclosed `${{{{` in template `{template}`"),
                "close the expression with `}}`",
            ));
        };
        parts.push(TemplatePart::Expression(after[..end].trim().to_owned()));
        rest = &after[end + 2..];
    }
    if !rest.is_empty() {
        if rest.contains("}}") {
            return Err(schema(
                path,
                format!("stray `}}}}` in template `{template}`"),
                "remove the stray closing braces",
            ));
        }
        parts.push(TemplatePart::Text(rest.to_owned()));
    }
    Ok(parts)
}

fn translate_expression(expression: &str, path: &str) -> Result<String, MigrateError> {
    let (body, default) = split_default(expression);
    let body = body.trim();
    if body.contains("!!") {
        return Err(MigrateError::UnsupportedExpression {
            path: path.to_owned(),
            expr: expression.to_owned(),
        });
    }
    let mut translated = if let Some(key) = body.strip_prefix("secrets::") {
        format!("secret({key})")
    } else if let Some(key) = body.strip_prefix("vars::") {
        format!("var({key})")
    } else if let Some(rest) = body.strip_prefix("resources::pg_databases.") {
        entity_reference(rest, expression, path)?
    } else if let Some(rest) = body.strip_prefix("resources::redis_clusters.") {
        entity_reference(rest, expression, path)?
    } else if let Some(rest) = body.strip_prefix("resources::app.") {
        format!("app.{rest}")
    } else if let Some(rest) = body.strip_prefix("domains::") {
        entity_reference(rest, expression, path)?
    } else if let Some(rest) = body.strip_prefix("components::") {
        entity_reference(rest, expression, path)?
    } else if let Some(rest) = body.strip_prefix("deployment::") {
        format!("deployment.{rest}")
    } else {
        return Err(MigrateError::UnsupportedExpression {
            path: path.to_owned(),
            expr: expression.to_owned(),
        });
    };
    if let Some(default) = default {
        translated.push_str(" ?? ");
        translated.push_str(default.trim());
    }
    Ok(translated)
}

/// `<id>.<prop>` out of `resources::…` remains `<id>.<prop>` in a `Forkfile`.
fn entity_reference(rest: &str, expression: &str, path: &str) -> Result<String, MigrateError> {
    let mut segments = rest.split('.');
    let id = segments.next().unwrap_or_default();
    let property = segments.collect::<Vec<_>>().join(".");
    if id.is_empty() || property.is_empty() {
        return Err(MigrateError::UnsupportedExpression {
            path: path.to_owned(),
            expr: expression.to_owned(),
        });
    }
    Ok(format!("{id}.{property}"))
}

fn split_default(expression: &str) -> (&str, Option<&str>) {
    let mut quote: Option<char> = None;
    let bytes = expression.as_bytes();
    let mut index = 0;
    while index + 1 < bytes.len() {
        let ch = bytes[index] as char;
        match quote {
            Some(active) if ch == active => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '?' && bytes[index + 1] == b'?' => {
                return (&expression[..index], Some(&expression[index + 2..]));
            }
            None => {}
        }
        index += 1;
    }
    (expression, None)
}
