//! Declared `database <name>` child databases inside the world's live postgres pod.
//!
//! `initdb` only creates the `postgres` maintenance database, so every `database <db>` in a
//! `Forkfile` is reconciled here: once the pod's unix socket answers and strictly before any
//! `pre-deploy` hook may run, through `crun exec` inside the running pod. The databases live in
//! the per-world `PGDATA` clone, so `slipfork fork` carries them without another round trip.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::control::{self, OutputKind, Stream};
use crate::error::{Error, Result};
use crate::resources::binary_in;

use super::launcher::PostgresLauncher;
use super::{PGDATA, SOCKET_DIR};

/// The database `initdb` always creates; it never needs a reconcile round trip.
pub const DEFAULT_DATABASE: &str = "postgres";
/// Rootfs-relative `psql` locations, matching the `initdb`/`postgres` image layouts.
const PSQL_CANDIDATES: [&str; 4] = [
    "usr/lib/postgresql/18/bin/psql",
    "usr/local/pgsql/bin/psql",
    "usr/local/bin/psql",
    "usr/bin/psql",
];
/// `NAMEDATALEN - 1`: PostgreSQL silently truncates longer identifiers.
const MAX_NAME_BYTES: usize = 63;

/// A `database <name>` identifier, validated with the `Forkfile` lexer's identifier rule.
///
/// Keeping the type here means the SQL below can quote a name that cannot contain a quote,
/// backslash, whitespace, or shell metacharacter; it still travels as one argv word to `psql`,
/// never through a shell.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DatabaseName(String);

impl DatabaseName {
    /// Parses one `database <name>` id: `[A-Za-z_][A-Za-z0-9_-]*`, at most 63 bytes.
    ///
    /// The `Forkfile` lexer already enforces this shape for every identifier token; the check is
    /// repeated here so no caller can hand unvalidated text to `psql`.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let mut bytes = raw.bytes();
        let head = bytes.next()?;
        if !(head.is_ascii_alphabetic() || head == b'_') {
            return None;
        }
        if raw.len() > MAX_NAME_BYTES {
            return None;
        }
        if !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-') {
            return None;
        }
        Some(Self(raw.to_owned()))
    }

    /// The validated identifier, unscathed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// One-row probe: `psql -tAc` prints `1` only when the database already exists.
    #[must_use]
    pub fn exists_query(&self) -> String {
        format!("SELECT 1 FROM pg_database WHERE datname = '{}'", self.0)
    }

    /// `CREATE DATABASE "<name>"`; a validated identifier cannot escape the double quotes.
    #[must_use]
    pub fn create_statement(&self) -> String {
        format!("CREATE DATABASE \"{}\"", self.0)
    }
}

/// Why a declared database could not be reconciled inside its pod.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseFault {
    database: String,
    detail: String,
    pod: String,
}

impl DatabaseFault {
    fn new(name: &DatabaseName, pod: &str, detail: impl Into<String>) -> Self {
        Self {
            database: name.as_str().to_owned(),
            detail: detail.into(),
            pod: pod.to_owned(),
        }
    }

    /// The one concrete next action for this failure.
    #[must_use]
    pub fn remedy(&self) -> String {
        format!("run `slipfork logs {}`", self.pod)
    }
}

impl fmt::Display for DatabaseFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "database `{}` could not be prepared inside `{}`: {}",
            self.database, self.pod, self.detail
        )
    }
}

/// The declared databases of one postgres pod plus the in-pod `psql` that reconciles them.
#[derive(Debug, Clone)]
pub struct DatabaseBootstrap {
    pods_root: PathBuf,
    pod: String,
    psql: String,
    databases: Vec<DatabaseName>,
}

impl DatabaseBootstrap {
    /// Plans the reconcile step for one pod; `None` when no non-default database is declared.
    ///
    /// Default (`postgres`) and duplicate declarations cost zero extra processes. A missing
    /// `psql` fails closed instead of shipping a world whose declared database does not exist.
    pub fn new(
        pods_root: &Path,
        pod: &str,
        rootfs: &Path,
        declared: &[String],
    ) -> Result<Option<Self>> {
        let mut databases = Vec::new();
        for raw in declared {
            let name = DatabaseName::parse(raw).ok_or_else(|| Error::InvalidPod {
                id: pod.to_owned(),
                reason: format!("database `{raw}` is not a valid identifier"),
            })?;
            if name.as_str() == DEFAULT_DATABASE || databases.contains(&name) {
                continue;
            }
            databases.push(name);
        }
        if databases.is_empty() {
            return Ok(None);
        }
        let psql = binary_in(rootfs, &PSQL_CANDIDATES).ok_or_else(|| Error::MissingTool {
            tool: "psql",
            path: rootfs.join(PSQL_CANDIDATES[0]),
        })?;
        Ok(Some(Self {
            pods_root: pods_root.to_path_buf(),
            pod: pod.to_owned(),
            psql,
            databases,
        }))
    }

    /// Creates every missing declared database; an existing one costs exactly one `psql` probe.
    ///
    /// Idempotent: a warm boot or a `fork` clone finds the database and issues no `CREATE`.
    pub fn ensure(&self) -> std::result::Result<(), DatabaseFault> {
        for name in &self.databases {
            if self.exists(name)? {
                continue;
            }
            let run = self.psql(name, &name.create_statement())?;
            if run.code != 0 {
                return Err(DatabaseFault::new(
                    name,
                    &self.pod,
                    format!("`CREATE DATABASE` exited with {}{}", run.code, run.tail()),
                ));
            }
        }
        Ok(())
    }

    fn exists(&self, name: &DatabaseName) -> std::result::Result<bool, DatabaseFault> {
        let run = self.psql(name, &name.exists_query())?;
        if run.code != 0 {
            return Err(DatabaseFault::new(
                name,
                &self.pod,
                format!(
                    "`psql` could not read `pg_database` (exit {}){}",
                    run.code,
                    run.tail()
                ),
            ));
        }
        Ok(run.stdout.trim() == "1")
    }

    /// Runs one statement through the pod's own `psql` over the readiness-proven unix socket.
    fn psql(
        &self,
        name: &DatabaseName,
        statement: &str,
    ) -> std::result::Result<PsqlRun, DatabaseFault> {
        let argv = [
            self.psql.clone(),
            "-X".to_owned(),
            "-h".to_owned(),
            format!("{PGDATA}/{SOCKET_DIR}"),
            "-p".to_owned(),
            PostgresLauncher::PORT.to_string(),
            "-U".to_owned(),
            "postgres".to_owned(),
            "-d".to_owned(),
            DEFAULT_DATABASE.to_owned(),
            "-tAc".to_owned(),
            statement.to_owned(),
        ];
        let mut stdout = String::new();
        let mut stderr = String::new();
        let code = control::exec(self.pods_root.as_path(), &self.pod, &argv, &mut |event| {
            if event.kind != OutputKind::Line {
                return;
            }
            match event.stream {
                Stream::Stdout => {
                    stdout.push_str(&event.line);
                    stdout.push('\n');
                }
                Stream::Stderr => {
                    stderr.push_str(&event.line);
                    stderr.push('\n');
                }
                Stream::Combined => {}
            }
        })
        .map_err(|error| DatabaseFault::new(name, &self.pod, first_line(&error.to_string())))?;
        Ok(PsqlRun {
            code,
            stdout,
            stderr,
        })
    }
}

/// One finished `psql` invocation inside the pod.
struct PsqlRun {
    code: i32,
    stdout: String,
    stderr: String,
}

impl PsqlRun {
    /// The error tail as one space-joined line, ready for a one-line + `next:` failure message.
    fn tail(&self) -> String {
        let lines = self
            .stderr
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        if lines.is_empty() {
            String::new()
        } else {
            format!(": {}", lines.join(" | "))
        }
    }
}

/// Collapses a multi-line protocol/`crun` error into its first informative line.
fn first_line(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(message)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_validated_and_quoted_as_one_argv_word() {
        let name = DatabaseName::parse("app_db").expect("a valid identifier");
        assert_eq!(
            name.exists_query(),
            "SELECT 1 FROM pg_database WHERE datname = 'app_db'"
        );
        assert_eq!(name.create_statement(), "CREATE DATABASE \"app_db\"");
        assert_eq!(
            DatabaseName::parse("app-db")
                .expect("the Forkfile lexer allows `-`")
                .create_statement(),
            "CREATE DATABASE \"app-db\""
        );
        for rejected in [
            "", "1app", "app db", "app'db", "app\"db", "app.db", "app;drop",
        ] {
            assert!(
                DatabaseName::parse(rejected).is_none(),
                "`{rejected}` must never reach SQL"
            );
        }
        assert!(
            DatabaseName::parse(&"a".repeat(MAX_NAME_BYTES + 1)).is_none(),
            "PostgreSQL would silently truncate it"
        );
    }

    #[test]
    fn only_default_databases_plan_no_process() {
        let dir = tempfile::tempdir().expect("disk temp dir");
        let declared = vec![DEFAULT_DATABASE.to_owned(), DEFAULT_DATABASE.to_owned()];
        let planned =
            DatabaseBootstrap::new(dir.path(), "pg", dir.path(), &declared).expect("validation");
        assert!(planned.is_none(), "a default-only postgres needs no `psql`");
    }
}
