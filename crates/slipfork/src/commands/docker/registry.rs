//! Registry credentials and the cached-image inventory: `login`, `logout`, `list`, `images`.
//!
//! The store is `<config>/slipfork/registries.json` (mode `0600`). A secret reaches it only after
//! `RegistryClient::login` accepted it; stdout, stderr, errors, and JSON never carry the secret.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead};
use std::path::Path;
use std::process::ExitCode;

use serde_json::json;
use slipfork_image::{ImageLayout, RegistryClient, canonical_host};
use slipfork_world::StoreLayout;

use crate::commands::secret::EchoGuard;
use crate::commands::{Session, emit};
use crate::ui::report::Palette;
use crate::ui::report::{Column, Table};
use crate::ui::report::{StreamKind, digest_cell};

use super::{DockerArgs, DockerVerb};

const LIST_COLUMNS: [Column; 2] = [Column::left(), Column::left()];
const IMAGE_COLUMNS: [Column; 3] = [Column::left(), Column::left(), Column::left()];

impl Session {
    /// Verifies one registry login, then writes the owner-only store.
    pub(super) fn docker_login(&self, args: &DockerArgs) -> ExitCode {
        let DockerVerb::Login {
            host,
            username,
            password_stdin,
        } = &args.verb
        else {
            unreachable!("docker_login is only reachable through DockerVerb::Login")
        };
        if username.trim().is_empty() {
            return self.report(
                "missing-username",
                "registry login needs a non-empty username",
                "pass `--username <user>` and retry",
                args.json,
            );
        }
        let host = canonical_host(host);
        let secret = match self.docker_password(&host, *password_stdin) {
            Ok(secret) => secret,
            Err(message) => {
                return self.report(
                    "registry-input",
                    &message,
                    &format!(
                        "pipe a token: printf '%s\\n' \"$TOKEN\" | slipfork docker login {host} --username {username} --password-stdin"
                    ),
                    args.json,
                );
            }
        };
        if secret.is_empty() {
            return self.report(
                "empty-password",
                "the registry password is empty",
                &format!(
                    "pipe a token: printf '%s\\n' \"$TOKEN\" | slipfork docker login {host} --username {username} --password-stdin"
                ),
                args.json,
            );
        }

        let mut credentials = self.host().registry_credentials.clone();
        if let Err(error) = credentials.set(&host, username.clone(), secret) {
            return self.report(
                "registry-host",
                &format!("cannot record credentials for `{host}`: {error}"),
                "use a registry host such as `ghcr.io` or `docker.io`",
                args.json,
            );
        }
        let client = RegistryClient::new(credentials.clone(), user_agent());
        if let Err(error) = client.login(&host) {
            return self.report(
                "registry-login",
                &format!("cannot sign in to `{host}`: {}", error.cause()),
                "check the host, username, and access token, then retry",
                args.json,
            );
        }

        let path = self.registry_store();
        if let Err(error) = credentials.save(&path) {
            return self.report(
                "registry-store",
                &format!("cannot save `{}`: {}", path.display(), error.cause()),
                "make the Slipfork config directory writable, then retry",
                args.json,
            );
        }
        if args.json {
            return emit(&format!(
                "{}\n",
                json!({ "host": host, "username": username, "stored": true })
            ));
        }
        let palette = Palette::new(self.color());
        emit(&format!(
            "{} Logged in to {} as {}\n",
            palette.icon_ok(),
            palette.value(&host),
            palette.kind(username)
        ))
    }

    /// Removes one host from the Slipfork-owned store.
    pub(super) fn docker_logout(&self, args: &DockerArgs) -> ExitCode {
        let DockerVerb::Logout { host } = &args.verb else {
            unreachable!("docker_logout is only reachable through DockerVerb::Logout")
        };
        let host = canonical_host(host);
        let owned = self
            .host()
            .registry_credentials
            .hosts()
            .any(|(stored, _)| stored == host);
        let mut credentials = self.host().registry_credentials.clone();
        credentials.remove(&host);
        let path = self.registry_store();
        if let Err(error) = credentials.save(&path) {
            return self.report(
                "registry-store",
                &format!("cannot save `{}`: {}", path.display(), error.cause()),
                "make the Slipfork config directory writable, then retry",
                args.json,
            );
        }
        if args.json {
            return emit(&format!("{}\n", json!({ "host": host, "removed": owned })));
        }
        let palette = Palette::new(self.color());
        if owned {
            emit(&format!(
                "{} Removed credentials for {}\n",
                palette.icon_ok(),
                palette.value(&host)
            ))
        } else {
            emit(&format!(
                "No Slipfork credentials stored for {}\n",
                palette.value(&host)
            ))
        }
    }

    /// Lists only the Slipfork-owned hosts; Docker's fallback is implicit.
    pub(super) fn docker_list(&self, args: &DockerArgs) -> ExitCode {
        let entries: Vec<(String, String)> = self
            .host()
            .registry_credentials
            .hosts()
            .map(|(host, username)| (host.to_owned(), username.to_owned()))
            .collect();
        if args.json {
            let rows: Vec<_> = entries
                .iter()
                .map(|(host, username)| json!({ "host": host, "username": username }))
                .collect();
            return emit(&format!("{}\n", serde_json::Value::Array(rows)));
        }
        if entries.is_empty() {
            return emit("No stored registries\n");
        }
        let palette = Palette::new(self.color());
        let table = Table::new(&LIST_COLUMNS).gap("  ");
        let mut rows = vec![vec!["HOST".to_owned(), "USERNAME".to_owned()]];
        rows.extend(
            entries
                .iter()
                .map(|(host, username)| vec![palette.value(host), palette.kind(username)]),
        );
        let widths = table.widths(&rows);
        let mut text = String::new();
        text.push_str(&table.line(&widths, &[palette.meta("HOST"), palette.meta("USERNAME")]));
        text.push('\n');
        for row in rows.iter().skip(1) {
            text.push_str(&table.line(&widths, row));
            text.push('\n');
        }
        emit(&text)
    }

    /// Lists pulled OCI references plus components recorded by `docker build`.
    pub(super) fn docker_images(&self, args: &DockerArgs) -> ExitCode {
        let root = self.images_root();
        let layout = ImageLayout::new(root.clone());
        let refs_path = layout.refs();
        let refs: BTreeMap<String, String> = match read_index(&refs_path) {
            Ok(Some(refs)) => refs,
            Ok(None) => BTreeMap::new(),
            Err(message) => {
                return self.report(
                    "image-refs",
                    &message,
                    &format!("delete `{}` and pull the image again", refs_path.display()),
                    args.json,
                );
            }
        };
        let built_path = root.join("built.json");
        let built: BTreeMap<String, serde_json::Value> = match read_index(&built_path) {
            Ok(Some(built)) => built,
            Ok(None) => BTreeMap::new(),
            Err(message) => {
                return self.report(
                    "image-index",
                    &message,
                    &format!(
                        "delete `{}` and rebuild the component",
                        built_path.display()
                    ),
                    args.json,
                );
            }
        };

        let mut json_rows = Vec::new();
        let mut rows = vec![vec![
            "SOURCE".to_owned(),
            "NAME".to_owned(),
            "IMAGE".to_owned(),
        ]];
        for (reference, digest) in &refs {
            if !layout.image(digest).exists() {
                continue;
            }
            rows.push(vec!["pull".to_owned(), reference.clone(), digest.clone()]);
            json_rows.push(json!({
                "source": "pull",
                "reference": reference,
                "digest": digest,
            }));
        }
        for (component, entry) in &built {
            let image_id = entry
                .get("image_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?");
            rows.push(vec![
                "build".to_owned(),
                component.clone(),
                image_id.to_owned(),
            ]);
            let mut value = json!({
                "source": "build",
                "component": component,
                "image_id": image_id,
            });
            for key in ["target", "digest"] {
                if let Some(item) = entry.get(key) {
                    value[key] = item.clone();
                }
            }
            json_rows.push(value);
        }

        if args.json {
            return emit(&format!("{}\n", serde_json::Value::Array(json_rows)));
        }
        if rows.len() == 1 {
            return emit("No OCI images cached or built in this store\n");
        }
        let palette = Palette::new(self.color());
        let table = Table::new(&IMAGE_COLUMNS).gap("  ");
        let mut measured = vec![vec![
            palette.meta("SOURCE"),
            palette.meta("NAME"),
            palette.meta("IMAGE"),
        ]];
        measured.extend(rows.iter().skip(1).map(|row| {
            vec![
                palette.meta(&row[0]),
                palette.value(&row[1]),
                palette.meta(&digest_cell(&row[2])),
            ]
        }));
        let widths = table.widths(&measured);
        let mut text = String::new();
        for row in &measured {
            text.push_str(&table.line(&widths, row));
            text.push('\n');
        }
        emit(&text)
    }

    /// Reads one password line from stdin, or prompts without echo on a real terminal.
    fn docker_password(&self, host: &str, password_stdin: bool) -> Result<String, String> {
        if password_stdin {
            return read_one_line().map_err(|error| format!("cannot read stdin: {error}"));
        }
        if !self.stdin_is_terminal() {
            return Err(format!("no password supplied for `{host}`"));
        }
        let prompt = format!("password for {host}: ");
        let _ = self.renderer().emit_text(StreamKind::Stderr, &prompt);
        let _echo = EchoGuard::disable();
        let secret =
            read_one_line().map_err(|error| format!("cannot read the password: {error}"))?;
        let _ = self.renderer().emit_text(StreamKind::Stderr, "\n");
        Ok(secret)
    }

    /// `<config>/slipfork/registries.json`, resolved from the injected config root.
    fn registry_store(&self) -> std::path::PathBuf {
        self.host().config_dir.join("slipfork/registries.json")
    }

    /// The OCI cache root for this directory's project store.
    fn images_root(&self) -> std::path::PathBuf {
        StoreLayout::at(self.host().store_root_for(self.cwd())).images_root()
    }
}

fn read_index<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("cannot read `{}`: {error}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read `{}`: {error}", path.display())),
    }
}

fn read_one_line() -> io::Result<String> {
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\n', '\r']).to_owned())
}

fn user_agent() -> String {
    format!("slipfork/{}", env!("CARGO_PKG_VERSION"))
}
