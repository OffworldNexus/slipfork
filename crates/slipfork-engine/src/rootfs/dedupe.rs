//! Pure rootfs build deduplication for `Forkfile` services (`docs/design/forkfile.md` §3).
//!
//! Services that declare the same `./dir` build inputs — `(repo identity, repo-relative context,
//! dockerfile, stage, build_env)` — share one image build, and services pulling the same
//! `from <image>` digest share one extracted rootfs. A build key never contains a worktree path:
//! it is the one [`BuildKey`](slipfork_world::rootfs::BuildKey) formula from `slipfork-world`, so
//! a fork in another directory still hits the build its parent already produced.
//!
//! Nothing here touches disk, spawns a process, or reads the environment.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::fmt;

use sha2::{Digest, Sha256};
use slipfork_world::EngineError;
use slipfork_world::rootfs::{BuildEnvVar, BuildKey};

/// Domain tag separating pod image keys from every other digest in the engine.
const IMAGE_KEY_DOMAIN: &[u8] = b"slipfork-pod-image-key:v1\0";

/// One `./dir` service's declared build inputs.
///
/// Deliberately not `Debug`: `build_env` carries resolved (often secret) values, and the
/// [`RootfsKey`] is the only thing this module ever exposes.
#[derive(Clone, Copy)]
pub struct BuildSource<'a> {
    /// Stable identity of the declaring project (git-remote-derived slug or project id), never a
    /// worktree path.
    pub repo: &'a str,
    /// Declared repo-relative build context (`./api`), normalized — never an absolute directory.
    pub context: &'a str,
    /// Dockerfile path relative to the context (`Dockerfile` by default).
    pub dockerfile: &'a str,
    /// Named target stage; the empty string selects the final stage.
    pub stage: &'a str,
    /// Resolved build environment; values are hashed, never stored.
    pub build_env: &'a [BuildEnvVar],
}

/// Where one service's read-only root filesystem comes from.
#[derive(Clone, Copy)]
pub enum PodImageSource<'a> {
    /// A `service <id> ./dir` build.
    Build(BuildSource<'a>),
    /// A `service <id> from <image>` reference, identified by its immutable digest.
    Image {
        /// Upstream digest (`sha256:<64 lowercase hex>`).
        digest: &'a str,
    },
}

/// Deterministic identity of one rootfs build or image extraction.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RootfsKey(String);

impl RootfsKey {
    /// Key one `./dir` build through the shared
    /// [`BuildKey`](slipfork_world::rootfs::BuildKey) formula.
    ///
    /// The repo identity and the *declared* repo-relative context are load-bearing: hashing an
    /// absolute worktree path would split one build across forks and re-resolve caches per
    /// checkout.
    pub fn for_build(source: &BuildSource<'_>) -> Result<Self, EngineError> {
        if source.repo.trim().is_empty() {
            return Err(EngineError::InvalidWorldSpec {
                reason: "a service build needs a stable repo identity (the project slug)\n  fix: \
                         pass the git-remote-derived project identity, never an empty string or a \
                         worktree path"
                    .to_owned(),
            });
        }
        if source.context.is_empty() || source.context.starts_with('/') {
            return Err(EngineError::InvalidWorldSpec {
                reason: format!(
                    "build context '{}' must be a repo-relative directory such as `./api`\n  fix: \
                     declare `service <id> ./dir` relative to the Forkfile's repo root",
                    source.context
                ),
            });
        }
        if source.context.split('/').any(|component| component == "..") {
            return Err(EngineError::InvalidWorldSpec {
                reason: format!(
                    "build context '{}' escapes the repository root\n  fix: declare a context \
                     inside the repo, without `..` components",
                    source.context
                ),
            });
        }
        let mut build_env = source.build_env.to_vec();
        build_env.sort_by(|left, right| left.name.cmp(&right.name));
        if let Some(pair) = build_env
            .windows(2)
            .find(|pair| pair[0].name == pair[1].name)
        {
            return Err(EngineError::InvalidWorldSpec {
                reason: format!(
                    "build env '{}' is declared twice for the build at '{}'\n  fix: keep one \
                     value per build env name",
                    pair[0].name, source.context
                ),
            });
        }
        Ok(Self(
            BuildKey {
                project: source.repo,
                context: source.context,
                dockerfile: source.dockerfile,
                stage: source.stage,
                build_env: &build_env,
            }
            .hex(),
        ))
    }

    /// Key a `from <image>` service by its immutable digest.
    pub fn for_image(digest: &str) -> Result<Self, EngineError> {
        let hex_digest = digest
            .strip_prefix("sha256:")
            .filter(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
            .ok_or_else(|| EngineError::InvalidWorldSpec {
                reason: format!(
                    "image digest '{digest}' must be `sha256:<64 lowercase hex>`\n  fix: resolve \
                     the tag first (`docker image inspect --format '{{{{.Id}}}}' <image>`) and key \
                     the rootfs by that digest"
                ),
            })?;

        let mut hasher = Sha256::new();
        hasher.update(IMAGE_KEY_DOMAIN);
        hasher.update(hex_digest.as_bytes());
        Ok(Self(hex::encode(hasher.finalize())))
    }

    /// Full lowercase hex key.
    pub fn hex(&self) -> &str {
        &self.0
    }

    /// First 12 hex characters, the diagnostic form.
    pub fn short(&self) -> &str {
        &self.0[..12]
    }
}

impl fmt::Debug for RootfsKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RootfsKey({})", self.short())
    }
}

impl fmt::Display for RootfsKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.short())
    }
}

/// One distinct rootfs and the services that share it, in first-declared order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootfsEntry {
    key: RootfsKey,
    services: Vec<String>,
}

impl RootfsEntry {
    /// The shared rootfs key.
    pub fn key(&self) -> &RootfsKey {
        &self.key
    }

    /// Services that resolve to this one rootfs, in `Forkfile` declaration order.
    pub fn services(&self) -> &[String] {
        &self.services
    }
}

/// The deduplicated rootfs set of one evaluated `Forkfile`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootfsPlan {
    entries: Vec<RootfsEntry>,
}

impl RootfsPlan {
    /// Group every `(service_id, source)` pair by rootfs identity.
    ///
    /// Entries keep first-seen order and each entry lists its services in declaration order, so
    /// the plan is deterministic and a caller builds each distinct rootfs exactly once.
    pub fn plan<'a>(
        services: impl IntoIterator<Item = (&'a str, PodImageSource<'a>)>,
    ) -> Result<Self, EngineError> {
        let mut plan = Self::default();
        let mut seen = BTreeSet::new();
        for (service, source) in services {
            if service.is_empty() {
                return Err(EngineError::InvalidWorldSpec {
                    reason: "a planned rootfs service needs a non-empty service id\n  fix: name \
                             every `service <id>` block with a lowercase DNS label"
                        .to_owned(),
                });
            }
            if !seen.insert(service.to_owned()) {
                return Err(EngineError::InvalidWorldSpec {
                    reason: format!(
                        "service '{service}' is planned twice\n  fix: pass each service id once \
                         and let the plan group it under its shared rootfs"
                    ),
                });
            }
            let key = match source {
                PodImageSource::Build(build) => RootfsKey::for_build(&build)?,
                PodImageSource::Image { digest } => RootfsKey::for_image(digest)?,
            };
            match plan.entries.iter_mut().find(|entry| entry.key == key) {
                Some(entry) => entry.services.push(service.to_owned()),
                None => plan.entries.push(RootfsEntry {
                    key,
                    services: vec![service.to_owned()],
                }),
            }
        }
        Ok(plan)
    }

    /// Distinct rootfses, in first-seen order.
    pub fn entries(&self) -> &[RootfsEntry] {
        &self.entries
    }

    /// Number of rootfs materializations this plan needs.
    pub fn distinct(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envs(pairs: &[(&str, &str)]) -> Vec<BuildEnvVar> {
        pairs
            .iter()
            .map(|(name, value)| BuildEnvVar {
                name: (*name).to_owned(),
                value: (*value).to_owned(),
            })
            .collect()
    }

    fn build<'a>(
        repo: &'a str,
        context: &'a str,
        stage: &'a str,
        dockerfile: &'a str,
        build_env: &'a [BuildEnvVar],
    ) -> BuildSource<'a> {
        BuildSource {
            repo,
            context,
            dockerfile,
            stage,
            build_env,
        }
    }

    #[test]
    fn identical_build_inputs_share_one_key_and_declaration_order_is_irrelevant() {
        let first = envs(&[("NPM_TOKEN", "one"), ("TARGET", "release")]);
        let reordered = envs(&[("TARGET", "release"), ("NPM_TOKEN", "one")]);

        let key = RootfsKey::for_build(&build("acme", "./api", "app", "Dockerfile", &first))
            .expect("key");
        assert_eq!(
            key,
            RootfsKey::for_build(&build("acme", "./api", "app", "Dockerfile", &reordered))
                .expect("key"),
            "sorted build env must not split one build"
        );
        assert_eq!(key.hex().len(), 64);
        assert!(key.hex().bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(key.short(), &key.hex()[..12]);
    }

    #[test]
    fn the_key_is_worktree_independent_but_repo_and_build_inputs_are_not() {
        let plain = envs(&[]);
        let base = RootfsKey::for_build(&build("acme", "./api", "app", "Dockerfile", &plain))
            .expect("key");

        // The same declaration in another checkout (different repo identity spelling would be a
        // different project) keys identically — no worktree path is part of the formula.
        assert_eq!(
            base,
            RootfsKey::for_build(&build("acme", "./api", "app", "Dockerfile", &plain))
                .expect("key")
        );

        for changed in [
            RootfsKey::for_build(&build("other-repo", "./api", "app", "Dockerfile", &plain)),
            RootfsKey::for_build(&build("acme", "./web", "app", "Dockerfile", &plain)),
            RootfsKey::for_build(&build("acme", "./api", "worker", "Dockerfile", &plain)),
            RootfsKey::for_build(&build(
                "acme",
                "./api",
                "app",
                "docker/App.Dockerfile",
                &plain,
            )),
            RootfsKey::for_build(&build(
                "acme",
                "./api",
                "app",
                "Dockerfile",
                &envs(&[("TARGET", "release")]),
            )),
        ] {
            assert_ne!(
                changed.expect("key"),
                base,
                "each build input must distinguish a build"
            );
        }
    }

    #[test]
    fn a_build_key_never_collides_with_an_image_digest_and_secrets_never_render() {
        let secret = "npm_super_secret_value";
        let build_env = envs(&[("NPM_TOKEN", secret)]);
        let key = RootfsKey::for_build(&build("acme", "./api", "app", "Dockerfile", &build_env))
            .expect("build key");
        let digest = format!("sha256:{}", "a".repeat(64));
        let image = RootfsKey::for_image(&digest).expect("image key");

        assert_ne!(key, image);
        for rendered in [format!("{key:?}"), format!("{key}"), format!("{image:?}")] {
            assert!(
                !rendered.contains(secret),
                "a rootfs key must never render a build secret: {rendered}"
            );
        }
        assert!(matches!(
            RootfsKey::for_image("alpine:3.21"),
            Err(EngineError::InvalidWorldSpec { .. })
        ));
        assert!(matches!(
            RootfsKey::for_image(&format!("sha256:{}", "A".repeat(64))),
            Err(EngineError::InvalidWorldSpec { .. })
        ));
    }

    #[test]
    fn invalid_build_inputs_fail_closed_with_a_remedy() {
        let plain = envs(&[]);
        for source in [
            ("acme", "/abs/api"),
            ("acme", "../escape"),
            ("acme", ""),
            ("  ", "./api"),
        ] {
            let source = build(source.0, source.1, "app", "Dockerfile", &plain);
            match RootfsKey::for_build(&source) {
                Err(EngineError::InvalidWorldSpec { reason }) => {
                    assert!(reason.contains("fix: "), "missing remedy: {reason}")
                }
                other => panic!(
                    "build context '{}' must fail closed, got {other:?}",
                    source.context
                ),
            }
        }
        let duplicated = envs(&[("TARGET", "a"), ("TARGET", "b")]);
        match RootfsKey::for_build(&build("acme", "./api", "app", "Dockerfile", &duplicated)) {
            Err(EngineError::InvalidWorldSpec { reason }) => {
                assert!(reason.contains("fix: "), "missing remedy: {reason}")
            }
            other => panic!("duplicate build env must fail closed, got {other:?}"),
        }
    }

    #[test]
    fn the_plan_groups_shared_rootfses_in_first_seen_order() {
        let plain = envs(&[]);
        let digest = format!("sha256:{}", "b".repeat(64));
        let api = build("acme", "./api", "app", "Dockerfile", &plain);
        let plan = RootfsPlan::plan([
            ("api", PodImageSource::Build(api)),
            ("worker", PodImageSource::Build(api)),
            ("db", PodImageSource::Image { digest: &digest }),
            (
                "web",
                PodImageSource::Build(build("acme", "./web", "app", "Dockerfile", &plain)),
            ),
        ])
        .expect("valid plan");

        assert_eq!(plan.distinct(), 3);
        assert_eq!(
            plan.entries()[0].services().to_vec(),
            vec!["api".to_owned(), "worker".to_owned()]
        );
        assert_eq!(plan.entries()[1].services().to_vec(), vec!["db".to_owned()]);
        assert_eq!(
            plan.entries()[2].services().to_vec(),
            vec!["web".to_owned()]
        );
        assert_eq!(
            plan.entries()[0].key(),
            &RootfsKey::for_build(&api).expect("key")
        );
    }

    #[test]
    fn duplicate_service_ids_and_empty_ids_are_refused() {
        let plain = envs(&[]);
        let source = build("acme", "./api", "app", "Dockerfile", &plain);
        assert!(RootfsPlan::plan([("api", PodImageSource::Build(source))]).is_ok());
        assert!(matches!(
            RootfsPlan::plan([
                ("api", PodImageSource::Build(source)),
                ("api", PodImageSource::Build(source))
            ]),
            Err(EngineError::InvalidWorldSpec { .. })
        ));
        assert!(matches!(
            RootfsPlan::plan([("", PodImageSource::Build(source))]),
            Err(EngineError::InvalidWorldSpec { .. })
        ));
    }
}
