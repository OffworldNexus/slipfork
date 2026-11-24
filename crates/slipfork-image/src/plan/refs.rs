//! The one owner of `images/refs.json`: warm `FROM`/`COPY --from` references never hit a registry.
//!
//! A warm resolve reads the remembered tag → digest map and the cached OCI config beside it. First
//! sight (and every `Cache::Cold`) resolves the manifest once through [`OciPuller`] and remembers
//! the digest. `FROM scratch` never reaches this type.

use slipfork_world::ImageFacts;

use crate::error::Result;
use crate::layout::ImageLayout;
use crate::puller::OciPuller;
use crate::reference::ImageReference;

/// A pinned reference: its manifest digest plus the OCI config a stage inherits.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// `sha256:<hex>` of the per-architecture manifest.
    pub digest: String,
    /// `config.Env`, `config.Entrypoint`, and `config.Cmd` of the base image.
    pub facts: ImageFacts,
}

/// Resolves and remembers every floating image reference for one build.
pub struct RefResolver<'a> {
    puller: &'a OciPuller,
    layout: ImageLayout,
    cold: bool,
}

impl<'a> RefResolver<'a> {
    /// Binds the resolver to one puller, one `<images>` root, and the build cache mode.
    #[must_use]
    pub fn new(
        puller: &'a OciPuller,
        images_root: impl Into<std::path::PathBuf>,
        cold: bool,
    ) -> Self {
        Self {
            puller,
            layout: ImageLayout::new(images_root),
            cold,
        }
    }

    /// Resolves `reference`; warm resolves the remembered digest with no network.
    pub fn resolve(&self, reference: &ImageReference) -> Result<Resolved> {
        let key = reference.to_string();
        if !self.cold
            && let Some(digest) = self.layout.cached_ref(&key)
            && let Some(facts) = self.puller.cached_facts(&digest)
        {
            return Ok(Resolved { digest, facts });
        }
        let (digest, facts) = self.puller.resolve_image(reference)?;
        self.layout.remember_ref(&key, &digest);
        Ok(Resolved { digest, facts })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use slipfork_world::{BtrfsEngine, StoreLayout, WorldEngine};

    use super::RefResolver;
    use crate::layout::ImageLayout;
    use crate::puller::OciPuller;
    use crate::reference::ImageReference;
    use crate::registry::RegistryScheme;
    use crate::{Architecture, RegistryCredentials};

    fn puller(root: &std::path::Path) -> (OciPuller, ImageLayout) {
        let layout = ImageLayout::new(root.join("images"));
        layout.ensure().expect("create the image cache root");
        let engine: Arc<dyn WorldEngine> =
            Arc::new(BtrfsEngine::new(StoreLayout::at(root.join("store"))));
        let puller = OciPuller::new(
            engine,
            layout.clone(),
            RegistryCredentials::default(),
            Architecture::X86_64,
            "slipfork-tests/0.1".to_owned(),
        )
        .with_scheme(RegistryScheme::Http);
        (puller, layout)
    }

    #[test]
    fn a_warm_reference_reads_the_remembered_digest_without_network_and_cold_does_not() {
        let root = std::path::PathBuf::from("/var/tmp").join(format!(
            "slipfork-refs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos())
        ));
        std::fs::create_dir_all(&root).expect("create the refs lab");
        let (puller, layout) = puller(&root);
        let reference =
            ImageReference::parse("127.0.0.1:1/library/demo:latest").expect("reference parses");
        let digest = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        layout.remember_ref(&reference.to_string(), digest);
        std::fs::write(
            layout.config(digest),
            br#"{"config":{"Env":["A=B"],"Entrypoint":["/bin/sh"],"Cmd":[]}}"#,
        )
        .expect("write the cached config");

        let warm = RefResolver::new(&puller, root.join("images"), false);
        let resolved = warm
            .resolve(&reference)
            .expect("a warm reference never contacts the registry");
        assert_eq!(resolved.digest, digest);
        assert_eq!(resolved.facts.env, ["A=B".to_owned()]);

        let cold = RefResolver::new(&puller, root.join("images"), true);
        assert!(
            cold.resolve(&reference).is_err(),
            "`Cache::Cold` must re-resolve and fail against an unreachable registry"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
