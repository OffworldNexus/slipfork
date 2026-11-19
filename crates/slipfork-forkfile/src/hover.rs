//! Canonical hover text for `Forkfile` keywords, resources, properties, and `secret(...)` /
//! `var(...)` calls. Language servers only translate positions; the language knowledge stays here.

use crate::parser::parse_forkfile;

/// The identifier-like word surrounding a byte offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// The word text (`app_db.url`, `pre-deploy`, `secret`).
    pub text: String,
    /// Byte offset of the first byte.
    pub start: usize,
    /// Byte offset one past the last byte.
    pub end: usize,
}

/// The word under `offset`, expanding over identifier, dotted, dashed, and path characters.
pub fn word_at(source: &str, offset: usize) -> Option<Word> {
    if offset > source.len() || !source.is_char_boundary(offset) {
        return None;
    }
    let bytes = source.as_bytes();
    let mut start = offset;
    while start > 0 && is_word_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = offset;
    while end < bytes.len() && is_word_byte(bytes[end]) {
        end += 1;
    }
    (start < end).then(|| Word {
        text: source[start..end].to_owned(),
        start,
        end,
    })
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/')
}

/// Markdown hover text for the word under `offset`, or `None` when nothing is documented.
pub fn hover(source: &str, offset: usize) -> Option<String> {
    let word = word_at(source, offset)?;
    if let Some(callee) = enclosing_call(source, word.start)
        && let Some(remedy) = call_doc(&callee)
    {
        return Some(format!("`{callee}({key})` — {remedy}", key = word.text));
    }
    if let Some(text) = describe_exact(&word.text) {
        return Some(text.to_owned());
    }
    let file = parse_forkfile(source).ok()?;
    if let Some((receiver, property)) = word.text.rsplit_once('.')
        && file.entity_kind(receiver).is_some()
        && let Some(text) = describe_property(property)
    {
        return Some(text.to_owned());
    }
    file.entity_kind(&word.text)
        .map(|kind| format!("**`{}`** — {kind} declared in this Forkfile.", word.text))
}

fn enclosing_call(source: &str, start: usize) -> Option<String> {
    let bytes = source.as_bytes();
    let mut cursor = start;
    while cursor > 0 && bytes[cursor - 1].is_ascii_whitespace() {
        cursor -= 1;
    }
    if cursor == 0 || bytes[cursor - 1] != b'(' {
        return None;
    }
    let mut open = cursor - 1;
    while open > 0 && bytes[open - 1].is_ascii_whitespace() {
        open -= 1;
    }
    let mut name_start = open;
    while name_start > 0
        && (bytes[name_start - 1].is_ascii_alphanumeric() || bytes[name_start - 1] == b'_')
    {
        name_start -= 1;
    }
    (name_start < open).then(|| source[name_start..open].to_owned())
}

fn call_doc(callee: &str) -> Option<&'static str> {
    match callee {
        "secret" => Some("resolved from the project's `0600` vault at boot and never printed."),
        "var" => Some("read from the daemon-injected CI/OS environment at boot."),
        "random_hex" | "random_base64" | "random_string" | "uuid4" => {
            Some("write-once fallback for `secret(<KEY>) ?? <generator>`.")
        }
        _ => None,
    }
}

fn describe_exact(word: &str) -> Option<&'static str> {
    Some(match word {
        "project" => "**`project <slug>`** — names the world and its `.<slug>.localhost` domains.",
        "features" => "**`features { ... }`** — declares feature flags and their implications.",
        "postgres" => {
            "**`postgres <cluster> { ... }`** — a PostgreSQL cluster; `version` picks the major."
        }
        "redis" => "**`redis <cluster> { ... }`** — a Redis cluster for the world.",
        "s3" => "**`s3 <bucket>`** — an S3-compatible bucket for the world.",
        "domain" => "**`domain <id>`** — a routed local hostname under `.localhost`.",
        "env" => "**`env <group> { KEY = <expr> }`** — reusable variables imported with `use env`.",
        "service" => {
            "**`service <id> <source> { ... }`** — one supervised container from a directory or OCI image."
        }
        "task" => "**`task <name> in <service>: ...`** — a named command run with `slipfork run`.",
        "on" => {
            "**`on <condition> { ... }`** — statements applied only when the feature expression holds."
        }
        "database" => {
            "**`database <id>`** — a database in a `postgres` cluster, referenced as `<id>.<prop>`."
        }
        "version" => "**`version <major>`** — pins the resource's major version.",
        "pool" => "**`pool = true|false`** — shares one database connection pool across services.",
        "dockerfile" => "**`dockerfile <path>`** — build file relative to the service context.",
        "stage" => "**`stage <name>`** — names the immutable image stage.",
        "size" => "**`size \"<hint>\"`** — a rootfs size hint such as `\"2G\"`.",
        "port" => "**`port <u16>`** — the single container port routed for this service.",
        "internal_ports" => {
            "**`internal_ports [<u16>, ...]`** — extra container ports never routed."
        }
        "route" => "**`route <prefix>`** — an ingress prefix (`/api`) or wildcard (`sub.*`).",
        "volume" => "**`volume <rel_path>`** — a writable path bind-mounted from the worktree.",
        "use" => "**`use env <group>`** — imports an `env` group into this service.",
        "build_env" => "**`build_env { ... }`** — variables passed to the image build only.",
        "run" => "**`run <argv...>`** — the service's long-running command.",
        "setup" => "**`setup:`** — commands run once before the service starts.",
        "pre-deploy" => "**`pre-deploy:`** — commands run before the service becomes routable.",
        "post-deploy" => "**`post-deploy:`** — commands run after the service is routable.",
        "failed-deploy" => "**`failed-deploy:`** — commands run when the service fails to boot.",
        "secret" => {
            "**`secret(<KEY>)`** — resolved from the project's `0600` vault at boot; never printed."
        }
        "var" => "**`var(<KEY>)`** — read from the daemon-injected CI/OS environment at boot.",
        "random_hex" => "**`random_hex(<bytes>)`** — write-once fallback of a missing secret.",
        "random_base64" => {
            "**`random_base64(<bytes>)`** — write-once fallback of a missing secret."
        }
        "random_string" => "**`random_string(<len>)`** — write-once fallback of a missing secret.",
        "uuid4" => "**`uuid4()`** — write-once UUID fallback of a missing secret.",
        _ => return None,
    })
}

fn describe_property(property: &str) -> Option<&'static str> {
    Some(match property {
        "url" => "**`.url`** — the connection or route URL for this resource.",
        "host" => "**`.host`** — the container hostname of this resource.",
        "port" => "**`.port`** — the port the resource listens on inside the world.",
        "user" => "**`.user`** — the login user for this resource.",
        "db_name" => "**`.db_name`** — the database name used on the wire.",
        "jdbc" => "**`.jdbc`** — the JDBC connection string for this database.",
        "endpoint" => "**`.endpoint`** — the S3-compatible endpoint URL.",
        "bucket" => "**`.bucket`** — the bucket name.",
        "region" => "**`.region`** — the S3 region.",
        "name" => "**`.name`** — the entity's canonical name.",
        "domain" => "**`.domain`** — the routed hostname of this service.",
        "id" => "**`.id`** — the deployment id.",
        "private_url" => "**`.private_url`** — the world-internal URL of this service.",
        "private_domain" => "**`.private_domain`** — the world-internal hostname.",
        "private_port" => "**`.private_port`** — the world-internal port.",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_entities_and_calls_have_hover_text() {
        let source = "postgres pg {\n    database app_db\n}\n";
        assert!(
            hover(source, 1)
                .expect("keyword")
                .contains("PostgreSQL cluster")
        );
        let offset = source.find("app_db").expect("database");
        assert!(
            hover(source, offset)
                .expect("entity")
                .contains("postgres database")
        );

        let secret = "env c {\n    K = secret(KEY)\n}\n";
        let offset = secret.find("KEY").expect("secret key");
        assert!(hover(secret, offset).expect("call").contains("0600"));
    }
}
