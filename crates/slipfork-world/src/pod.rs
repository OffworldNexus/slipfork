//! OCI pod values and the minimal non-root `config.json` that `crun` accepts.
//!
//! Pods inherit the world's `user`/`net`/`ipc` namespaces and create only `pid` + `mount` + `uts`
//! (no `network` stanza), so every pod shares the world's `127.0.0.1` and no pod can see the host
//! loopback. Bundles live on the mode-`0700` tmpfs runtime directory; `config.json` carries
//! runtime `env` and is mode `0600`, never argv.

use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{Error, NameKind, Result};
use crate::layout::validate_name;
use crate::secure_file::{ensure_private_dir, touch_private, write_private};

const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// OCI ids and hostnames share one lowercase bounded alphabet.
const MAX_ID: usize = 63;

/// Where a bind mount's source lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindSource {
    /// A host path (the project subvolume or a world data subvolume).
    Host(PathBuf),
    /// A file written into this pod's tmpfs bundle, named in [`PodSpec::files`].
    Bundle(String),
}

/// Whether a pod rootfs under `<pods>/<world>` is snapshotted with its world on `fork`.
///
/// Resource clones (`res-*`) and running services (`svc-*`) carry warm runtime caches; boot and
/// setup pods are ephemeral and must never travel into a fork.
#[must_use]
pub fn is_reusable_pod(id: &str) -> bool {
    id.starts_with("res-") || id.starts_with("svc-")
}

/// One bind mount into the pod rootfs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bind {
    /// Where the source lives.
    pub source: BindSource,
    /// Absolute in-container destination.
    pub destination: String,
    /// `true` mounts `ro`; `false` mounts `rw`.
    pub read_only: bool,
}

/// One runtime environment entry; `Debug` always redacts the value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodEnv {
    /// Variable name.
    pub key: String,
    /// Variable value (may be a secret; never logged).
    pub value: String,
}

impl PodEnv {
    /// Builds one `key=value` pair.
    #[must_use]
    pub fn new(key: &str, value: &str) -> Self {
        Self {
            key: key.to_owned(),
            value: value.to_owned(),
        }
    }
}

impl fmt::Debug for PodEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PodEnv")
            .field("key", &self.key)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

/// One tmpfs file written into the pod bundle before launch; `Debug` redacts contents.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodFile {
    /// Single-component file name inside the bundle.
    pub name: String,
    /// Exact contents (backend configs, bucket policies).
    pub contents: String,
}

impl fmt::Debug for PodFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PodFile")
            .field("name", &self.name)
            .field("contents", &"[REDACTED]")
            .finish()
    }
}

/// Everything needed to render one pod, as plain values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodSpec {
    /// OCI container id and per-pod UTS hostname.
    pub id: String,
    /// Writable CoW clone of the enclosing image subvolume.
    pub rootfs: PathBuf,
    /// Absolute working directory inside the pod.
    pub workdir: String,
    /// Entrypoint argv (never carries secrets).
    pub argv: Vec<String>,
    /// Environment inherited verbatim from the image config (`PATH`, `PG_MAJOR`, …).
    #[serde(default)]
    pub inherited_env: Vec<String>,
    /// Runtime environment, materialised only in tmpfs `config.json`.
    #[serde(default)]
    pub env: Vec<PodEnv>,
    /// Project/data subvolume and config binds.
    #[serde(default)]
    pub binds: Vec<Bind>,
    /// Files written into the bundle for [`BindSource::Bundle`].
    #[serde(default)]
    pub files: Vec<PodFile>,
}

impl PodSpec {
    /// Validates ids, absolute paths, env keys, bind destinations, and bundle file references.
    pub fn validate(&self) -> Result<()> {
        validate_name(NameKind::Pod, &self.id)?;
        if self.id.len() > MAX_ID {
            return Err(Error::InvalidPod {
                id: self.id.clone(),
                reason: format!("id longer than {MAX_ID} bytes"),
            });
        }
        if self.argv.is_empty() {
            return Err(Error::InvalidPod {
                id: self.id.clone(),
                reason: "argv is empty".to_owned(),
            });
        }
        if !self.rootfs.is_absolute() {
            return Err(Error::InvalidPod {
                id: self.id.clone(),
                reason: format!("rootfs `{}` is not absolute", self.rootfs.display()),
            });
        }
        absolute(&self.id, "workdir", &self.workdir)?;
        for pair in &self.inherited_env {
            let key = pair.split_once('=').map(|(key, _)| key).unwrap_or(pair);
            if key.is_empty() || key.contains('\u{0}') || pair.contains('\u{0}') {
                return Err(Error::InvalidPod {
                    id: self.id.clone(),
                    reason: format!("invalid inherited env `{pair}`"),
                });
            }
        }
        for env in &self.env {
            if env.key.is_empty()
                || env.key.contains('=')
                || env.key.contains('\u{0}')
                || env.value.contains('\u{0}')
            {
                return Err(Error::InvalidPod {
                    id: self.id.clone(),
                    reason: format!("invalid env key `{}`", env.key),
                });
            }
        }
        for file in &self.files {
            if !is_single_component(&file.name) {
                return Err(Error::InvalidPod {
                    id: self.id.clone(),
                    reason: format!("bundle file `{}` is not one component", file.name),
                });
            }
        }
        for bind in &self.binds {
            absolute(&self.id, "bind destination", &bind.destination)?;
            if let BindSource::Bundle(name) = &bind.source {
                if !self.files.iter().any(|file| &file.name == name) {
                    return Err(Error::InvalidPod {
                        id: self.id.clone(),
                        reason: format!("bind references undeclared bundle file `{name}`"),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Host values a pod config needs; injected by the supervisor, never read from the environment.
#[derive(Debug, Clone)]
pub struct PodHost {
    /// `crun` executable path.
    pub crun: PathBuf,
    /// Optional host `resolv.conf` bound read-only into each pod.
    pub resolv_conf: Option<PathBuf>,
    /// Invoking uid, identity-mapped inside the world user namespace.
    pub uid: u32,
    /// Invoking gid, identity-mapped inside the world user namespace.
    pub gid: u32,
}

/// A written tmpfs bundle: `<dir>/config.json` mode `0600` plus declared files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodBundle {
    /// The bundle directory (inside the world tmpfs pod root).
    pub dir: PathBuf,
    /// `<dir>/config.json`.
    pub config: PathBuf,
}

/// Writes the bundle for `spec` into `dir` and returns the config path.
pub fn write_bundle(spec: &PodSpec, host: &PodHost, dir: &Path) -> Result<PodBundle> {
    spec.validate()?;
    ensure_private_dir(dir)?;
    for file in &spec.files {
        write_private(&dir.join(&file.name), file.contents.as_bytes())?;
    }
    let passwd = dir.join("passwd");
    write_private(&passwd, passwd_contents(host.uid, host.gid).as_bytes())?;
    let hosts = dir.join("hosts");
    write_private(&hosts, hosts_contents(&spec.id).as_bytes())?;
    if spec.rootfs.join("etc").is_dir() && !spec.rootfs.join("etc/hosts").exists() {
        let _ = touch_private(&spec.rootfs.join("etc/hosts"));
    }
    // The host path only materialises the mountpoint; `process.cwd` must stay the *container*
    // path (`/project`), or the pod starts in an empty directory inside the rootfs clone.
    let workdir = rooted(&spec.rootfs, &spec.workdir);
    if !workdir.is_dir() {
        fs::create_dir_all(&workdir).map_err(|source| Error::io("create", &workdir, source))?;
    }
    for bind in &spec.binds {
        let destination = rooted(&spec.rootfs, &bind.destination);
        match &bind.source {
            BindSource::Host(_) => {
                if !destination.is_dir() {
                    fs::create_dir_all(&destination)
                        .map_err(|source| Error::io("create", &destination, source))?;
                }
            }
            BindSource::Bundle(_) => {
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|source| Error::io("create", parent, source))?;
                }
                touch_private(&destination)?;
            }
        }
    }
    let config = dir.join("config.json");
    let rendered = serde_json::to_vec_pretty(&oci_config(
        spec,
        host,
        dir,
        &passwd,
        &hosts,
        Path::new(&spec.workdir),
    ))
    .map_err(|source| Error::Json {
        path: config.clone(),
        source,
    })?;
    write_private(&config, &rendered)?;
    Ok(PodBundle {
        dir: dir.to_path_buf(),
        config,
    })
}

/// Image env first, the pod's own env last, so an own entry overrides the same key.
fn merged_env(spec: &PodSpec) -> Vec<String> {
    let mut ordered: Vec<(String, String)> = Vec::new();
    let mut upsert = |key: &str, value: &str| {
        if let Some(slot) = ordered.iter_mut().find(|(existing, _)| existing == key) {
            slot.1 = value.to_owned();
        } else {
            ordered.push((key.to_owned(), value.to_owned()));
        }
    };
    upsert("PATH", DEFAULT_PATH);
    for pair in &spec.inherited_env {
        if let Some((key, value)) = pair.split_once('=') {
            upsert(key, value);
        }
    }
    for entry in &spec.env {
        upsert(&entry.key, &entry.value);
    }
    ordered
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

/// Renders the minimal OCI config: world-inherited net/ipc, per-pod pid/mount/uts/user.
fn oci_config(
    spec: &PodSpec,
    host: &PodHost,
    bundle: &Path,
    passwd: &Path,
    hosts: &Path,
    workdir: &Path,
) -> serde_json::Value {
    let env = merged_env(spec);
    let mut mounts = vec![
        json!({"destination": "/proc", "type": "proc", "source": "proc"}),
        json!({
            "destination": "/dev",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "strictatime", "mode=755", "size=65536k"]
        }),
        json!({
            "destination": "/etc/passwd",
            "type": "bind",
            "source": passwd,
            "options": ["ro", "rbind"]
        }),
        json!({
            "destination": "/etc/hosts",
            "type": "bind",
            "source": hosts,
            "options": ["ro", "rbind"]
        }),
    ];
    if let Some(resolv_conf) = &host.resolv_conf {
        mounts.push(json!({
            "destination": "/etc/resolv.conf",
            "type": "bind",
            "source": resolv_conf,
            "options": ["ro", "rbind"]
        }));
    }
    for bind in &spec.binds {
        let source = match &bind.source {
            BindSource::Host(path) => path.clone(),
            BindSource::Bundle(name) => bundle.join(name),
        };
        let mut options = vec!["rbind"];
        if bind.read_only {
            options.push("ro");
        }
        mounts.push(json!({
            "destination": bind.destination,
            "type": "bind",
            "source": source,
            "options": options
        }));
    }
    json!({
        "ociVersion": "1.0.2",
        "process": {
            "terminal": false,
            "user": {"uid": host.uid, "gid": host.gid},
            "args": spec.argv,
            "env": env,
            "cwd": workdir,
            "noNewPrivileges": false
        },
        "root": {"path": spec.rootfs, "readonly": false},
        "hostname": spec.id,
        "mounts": mounts,
        "linux": {
            "namespaces": [
                {"type": "user"},
                {"type": "pid"},
                {"type": "mount"},
                {"type": "ipc"},
                {"type": "uts"}
            ],
            "uidMappings": [{"containerID": host.uid, "hostID": host.uid, "size": 1}],
            "gidMappings": [{"containerID": host.gid, "hostID": host.gid, "size": 1}],
            "maskedPaths": [],
            "readonlyPaths": []
        },
        "annotations": {"slipfork.pod": spec.id}
    })
}

/// `/etc/passwd` must contain `$UID`, or `postgres:18` `initdb` exits `1`.
fn passwd_contents(uid: u32, gid: u32) -> String {
    format!("root:x:0:0:root:/root:/sbin/nologin\nslipfork:x:{uid}:{gid}:slipfork:/:/bin/sh\n")
}

/// `/etc/hosts` maps `<hostname>.localhost`, `localhost`, and the pod's UTS `hostname` locally
/// so `socket.getfqdn("0.0.0.0")` resolves in microseconds without hitting `127.0.0.53` DNS.
fn hosts_contents(hostname: &str) -> String {
    format!(
        "127.0.0.1 {hostname}.localhost localhost {hostname}\n::1 {hostname}.localhost localhost ip6-localhost ip6-loopback {hostname}\n"
    )
}

fn rooted(rootfs: &Path, destination: &str) -> PathBuf {
    rootfs.join(destination.trim_start_matches('/'))
}

fn absolute(id: &str, label: &str, value: &str) -> Result<()> {
    let path = Path::new(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(Error::InvalidPod {
            id: id.to_owned(),
            reason: format!("{label} `{value}` must be absolute without `..`"),
        });
    }
    Ok(())
}

fn is_single_component(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}
