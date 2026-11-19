//! `Faceless` v2 template tokenizer and evaluator (`${{ ns::path ?? "default" }}`
//! and `${{ secrets::KEY !! random_string(50) }}`).

mod claimed;

pub use claimed::ClaimedEvaluator;

use base64::Engine;

use crate::model::{MissingRef, ResourceCoordinates, SnowError};
use crate::provider::{EntropySource, Namespace, SecretProvider, SecretValue};

const ALPHANUM: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// The one expression opener `Faceless` v2 recognizes. Tokenizing, pass-through forwarding, and
/// the [`has_expression`] probe all read it from here.
const EXPR_OPEN: &str = "${{";

/// Whether `template` still carries an unevaluated `Faceless` expression.
///
/// This is the single place that answers that question: callers that only need to reject an
/// unresolved value (a Dockerfile build argument, a child-process environment) delegate here
/// instead of scanning for the delimiter themselves.
pub(crate) fn has_expression(template: &str) -> bool {
    template.contains(EXPR_OPEN)
}

/// Supported `!!` secret generators in `Faceless` v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratorSpec {
    /// `random_string(N)` — `N` alphanumeric characters (`1..=512`).
    RandomString(usize),
    /// `random_hex(N)` — `N` random bytes encoded as `2N` lowercase hex chars (`1..=256`).
    RandomHex(usize),
    /// `random_base64(N)` — `N` random bytes encoded as URL-safe unpadded base64 (`1..=256`).
    RandomBase64(usize),
    /// `uuid4()` — RFC 4122 version 4 UUID.
    Uuid4,
}

impl GeneratorSpec {
    /// Parse a generator expression after `!!`.
    pub fn parse(raw: &str) -> Result<Self, SnowError> {
        let trimmed = raw.trim();
        if trimmed == "uuid4()" {
            return Ok(Self::Uuid4);
        }
        let (fn_name, arg_str) = trimmed
            .strip_suffix(')')
            .and_then(|s| s.split_once('('))
            .ok_or_else(|| SnowError::InvalidGenerator(trimmed.to_owned()))?;
        let n: usize = arg_str
            .trim()
            .parse()
            .map_err(|_| SnowError::InvalidGenerator(trimmed.to_owned()))?;
        match fn_name.trim() {
            "random_string" if (1..=512).contains(&n) => Ok(Self::RandomString(n)),
            "random_hex" if (1..=256).contains(&n) => Ok(Self::RandomHex(n)),
            "random_base64" if (1..=256).contains(&n) => Ok(Self::RandomBase64(n)),
            _ => Err(SnowError::InvalidGenerator(trimmed.to_owned())),
        }
    }

    /// Materialize a fresh random value using `entropy`.
    pub fn generate(&self, entropy: &dyn EntropySource) -> Result<String, SnowError> {
        match *self {
            Self::RandomString(len) => {
                // Rejection sampling avoids modulo bias across the 62-char alphabet (62 * 4 = 248).
                let mut out = String::with_capacity(len);
                let mut buf = [0u8; 64];
                while out.len() < len {
                    entropy.fill_bytes(&mut buf)?;
                    for &b in &buf {
                        if b < 248 {
                            out.push(ALPHANUM[(b % 62) as usize] as char);
                            if out.len() == len {
                                break;
                            }
                        }
                    }
                }
                Ok(out)
            }
            Self::RandomHex(bytes_len) => {
                let mut buf = vec![0u8; bytes_len];
                entropy.fill_bytes(&mut buf)?;
                Ok(hex::encode(buf))
            }
            Self::RandomBase64(bytes_len) => {
                let mut buf = vec![0u8; bytes_len];
                entropy.fill_bytes(&mut buf)?;
                Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
            }
            Self::Uuid4 => {
                let mut b = [0u8; 16];
                entropy.fill_bytes(&mut b)?;
                b[6] = (b[6] & 0x0f) | 0x40;
                b[8] = (b[8] & 0x3f) | 0x80;
                Ok(format!(
                    "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                    b[0],
                    b[1],
                    b[2],
                    b[3],
                    b[4],
                    b[5],
                    b[6],
                    b[7],
                    b[8],
                    b[9],
                    b[10],
                    b[11],
                    b[12],
                    b[13],
                    b[14],
                    b[15]
                ))
            }
        }
    }
}

/// Optional tail modifier of a `${{ ... }}` expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TailModifier {
    None,
    /// `?? "json_default"` — transient read-only fallback.
    Fallback(String),
    /// `!! <generator>` — write-once ensure into `secrets`.
    Ensure(GeneratorSpec),
}

/// One parsed token in a `Faceless` v2 template string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Literal(String),
    Expr {
        namespace: String,
        path: Vec<String>,
        modifier: TailModifier,
        raw: String,
    },
}

/// Tokenize and validate a `Faceless` v2 template string.
pub fn parse_faceless(template: &str) -> Result<Vec<Token>, SnowError> {
    let mut tokens = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find(EXPR_OPEN) {
        if start > 0 {
            tokens.push(Token::Literal(rest[..start].to_owned()));
        }
        let after_open = &rest[start + 3..];
        let end = after_open
            .find("}}")
            .ok_or_else(|| SnowError::UnclosedTemplate(template.to_owned()))?;
        let inner = after_open[..end].trim();
        tokens.push(parse_expr(inner)?);
        rest = &after_open[end + 2..];
    }
    if !rest.is_empty() {
        tokens.push(Token::Literal(rest.to_owned()));
    }
    Ok(tokens)
}

fn parse_expr(inner: &str) -> Result<Token, SnowError> {
    let (ns, rhs) = inner.split_once("::").ok_or_else(|| {
        if inner.contains('.') {
            SnowError::InvalidNamespaceSeparator(inner.to_owned())
        } else {
            SnowError::InvalidExpression(inner.to_owned())
        }
    })?;
    let namespace = ns.trim().to_owned();
    if namespace.is_empty() {
        return Err(SnowError::InvalidExpression(inner.to_owned()));
    }

    let (path_part, modifier) = if let Some((lhs, gen_raw)) = rhs.split_once("!!") {
        let gen_spec = GeneratorSpec::parse(gen_raw)?;
        (lhs.trim(), TailModifier::Ensure(gen_spec))
    } else if let Some((lhs, def_raw)) = rhs.split_once("??") {
        let parsed_default: String = serde_json::from_str(def_raw.trim())
            .map_err(|_| SnowError::InvalidDefaultLiteral(def_raw.trim().to_owned()))?;
        (lhs.trim(), TailModifier::Fallback(parsed_default))
    } else {
        (rhs.trim(), TailModifier::None)
    };

    let path: Vec<String> = path_part
        .split('.')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if path.is_empty() {
        return Err(SnowError::InvalidExpression(inner.to_owned()));
    }

    if matches!(modifier, TailModifier::Ensure(_)) && (namespace != "secrets" || path.len() != 1) {
        return Err(SnowError::EnsureOnlyAllowedOnSecrets(inner.to_owned()));
    }

    Ok(Token::Expr {
        namespace,
        path,
        modifier,
        raw: inner.to_owned(),
    })
}

/// Evaluate a template against `provider`, `resources`, and `entropy`, collecting missing refs.
///
/// This is the exhaustive resolver: every expression must resolve locally, and an absent value is
/// reported through `missing_out` (or [`SnowError::MissingValues`] by the caller) instead of being
/// forwarded. Local worlds use [`ClaimedEvaluator`] instead.
pub fn render_template(
    template: &str,
    env_location: &str,
    provider: &dyn SecretProvider,
    resources: &ResourceCoordinates,
    entropy: &dyn EntropySource,
    missing_out: &mut Vec<MissingRef>,
) -> Result<Option<SecretValue>, SnowError> {
    let mut policy = Policy::Exhaustive {
        env_location,
        missing_out,
    };
    let rendered = render(template, provider, resources, entropy, &mut policy)?;
    Ok((!rendered.missing).then_some(rendered.value))
}

/// How one render pass treats the expressions it meets.
enum Policy<'a> {
    /// Every expression resolves locally; an absent value is recorded, never forwarded.
    Exhaustive {
        env_location: &'a str,
        missing_out: &'a mut Vec<MissingRef>,
    },
    /// Only `claims` expressions resolve; every other expression passes through verbatim.
    Claimed {
        claims: &'a dyn Fn(&str, &[String]) -> bool,
    },
}

impl Policy<'_> {
    /// Whether this pass resolves `namespace::path` locally.
    fn resolves(&self, namespace: &str, path: &[String]) -> bool {
        match self {
            Self::Exhaustive { .. } => true,
            Self::Claimed { claims } => claims(namespace, path),
        }
    }

    /// Read one provider value, mapping the failure for this pass.
    fn read(
        &self,
        provider: &dyn SecretProvider,
        namespace: Namespace,
        key: &str,
    ) -> Result<Option<String>, SnowError> {
        match self {
            Self::Exhaustive { .. } => Ok(provider.get(namespace, key)?),
            Self::Claimed { .. } => {
                provider
                    .get(namespace, key)
                    .map_err(|source| SnowError::ClaimedSecretRead {
                        key: key.to_owned(),
                        source,
                    })
            }
        }
    }

    /// Persist one generated `!!` value, mapping the failure for this pass.
    fn persist(
        &self,
        provider: &dyn SecretProvider,
        key: &str,
        proposed: &str,
    ) -> Result<String, SnowError> {
        match self {
            Self::Exhaustive { .. } => Ok(provider.ensure_secret(key, proposed)?),
            Self::Claimed { .. } => provider.ensure_secret(key, proposed).map_err(|source| {
                SnowError::ClaimedSecretPersist {
                    key: key.to_owned(),
                    source,
                }
            }),
        }
    }

    /// Record a provider miss: `Ok(None)` means "counted as missing", `Ok(Some)` appends text.
    fn provider_missing(
        &mut self,
        namespace: &str,
        key: &str,
        raw: &str,
    ) -> Result<Option<String>, SnowError> {
        match self {
            Self::Exhaustive {
                env_location,
                missing_out,
            } => {
                missing_out.push(MissingRef {
                    expression: format!("{namespace}::{key}"),
                    referenced_by: (*env_location).to_owned(),
                });
                Ok(None)
            }
            Self::Claimed { .. } => Err(SnowError::ClaimedValueMissing {
                expression: raw.to_owned(),
            }),
        }
    }

    /// Fail on a coordinate miss (a `??` fallback is handled before this point).
    fn coordinate_missing(&self, raw: &str) -> Result<(), SnowError> {
        match self {
            Self::Exhaustive { .. } => Err(SnowError::UnknownResourcePath(raw.to_owned())),
            Self::Claimed { .. } => Err(SnowError::ClaimedCoordinateMissing {
                expression: raw.to_owned(),
            }),
        }
    }
}

/// One rendered value plus whether an exhaustive pass hit a missing reference.
struct Rendered {
    value: SecretValue,
    missing: bool,
}

/// The single token loop shared by [`render_template`] and [`ClaimedEvaluator`].
///
/// A `secrets::*` token taints the whole value, and `??` / `!!` keep their precedence, in **every**
/// pass: only the [`Policy`] decides what happens to an expression the pass cannot resolve.
fn render(
    template: &str,
    provider: &dyn SecretProvider,
    resources: &ResourceCoordinates,
    entropy: &dyn EntropySource,
    policy: &mut Policy<'_>,
) -> Result<Rendered, SnowError> {
    let mut rendered = String::with_capacity(template.len());
    let mut contaminated = false;
    let mut missing = false;

    for token in parse_faceless(template)? {
        match token {
            Token::Literal(literal) => rendered.push_str(&literal),
            Token::Expr {
                namespace,
                path,
                modifier,
                raw,
            } => {
                if !policy.resolves(&namespace, &path) {
                    // The remote resolver's reply is template-free, so it must receive the exact
                    // expression the author wrote — never a locally re-evaluated value.
                    rendered.push_str(EXPR_OPEN);
                    rendered.push(' ');
                    rendered.push_str(&raw);
                    rendered.push_str(" }}");
                    continue;
                }
                if let Some(ns) = Namespace::from_name(&namespace) {
                    if ns == Namespace::Secrets {
                        contaminated = true;
                    }
                    let key = path.join(".");
                    match policy.read(provider, ns, &key)? {
                        Some(value) => rendered.push_str(&value),
                        None => match modifier {
                            TailModifier::Fallback(default) => rendered.push_str(&default),
                            TailModifier::Ensure(generator) => {
                                let proposed = generator.generate(entropy)?;
                                rendered.push_str(&policy.persist(provider, &key, &proposed)?);
                            }
                            TailModifier::None => {
                                if let Some(text) =
                                    policy.provider_missing(&namespace, &key, &raw)?
                                {
                                    rendered.push_str(&text);
                                } else {
                                    missing = true;
                                }
                            }
                        },
                    }
                } else {
                    match resources.resolve(&namespace, &path) {
                        Some((value, secret)) => {
                            contaminated |= secret;
                            rendered.push_str(&value);
                        }
                        None => match modifier {
                            TailModifier::Fallback(default) => rendered.push_str(&default),
                            TailModifier::None | TailModifier::Ensure(_) => {
                                policy.coordinate_missing(&raw)?;
                            }
                        },
                    }
                }
            }
        }
    }

    Ok(Rendered {
        value: if contaminated {
            SecretValue::secret(rendered)
        } else {
            SecretValue::plain(rendered)
        },
        missing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_expression_recognizes_forwarded_templates_only() {
        // A non-claiming pass forwards the author's exact expression, spaces included.
        assert!(has_expression("${{ secrets::K }}"));
        assert!(has_expression("prefix ${{ vars::A.B }} suffix"));
        assert!(!has_expression("plain text"));
        assert!(!has_expression(""));
    }
}
