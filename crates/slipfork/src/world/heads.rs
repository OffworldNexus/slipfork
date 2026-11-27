//! The daemon-written sealed image plan one world boots from: `WorldHeads`.
//!
//! The daemon owns every pull and Dockerfile build; `__supervise` only reads the recorded
//! rootfs/config facts, so it never constructs a builder or pulls an image. A head is trusted
//! only when its build inputs still match disk and every rootfs is still a sealed Btrfs
//! subvolume; a stale parent fork fails closed instead of silently reusing an old image.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use slipfork_forkfile::FeatureSet;
use slipfork_forkfile::ast::Forkfile;
use slipfork_world::{
    BtrfsEngine, NodeKey, StoreLayout, WorldEngine, WorldName, WorldPlan, is_subvolume_path,
};

/// The on-disk heads schema version; an unknown version is stale and fails closed.
pub(crate) const HEADS_VERSION: u32 = 1;

/// One image's sealed rootfs plus the OCI metadata every pod derived from it inherits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ImageHead {
    /// Sealed content key of this image in the snapshot graph; the lease pins it.
    pub(crate) key: NodeKey,
    /// Read-only sealed rootfs subvolume.
    pub(crate) rootfs: PathBuf,
    /// Image `config.Env` entries, inherited verbatim.
    pub(crate) inherited_env: Vec<String>,
    /// `ENTRYPOINT` followed by `CMD`; empty when the image defines neither.
    pub(crate) argv: Vec<String>,
    /// Image `config.WorkingDir`.
    pub(crate) workdir: String,
    /// How this head was produced: `cached`, `built`, or `pulled`.
    pub(crate) state: String,
}

/// Everything `__supervise` may boot without constructing a builder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorldHeads {
    /// Schema version ([`HEADS_VERSION`]).
    pub(crate) v: u32,
    /// World id these heads were built for.
    pub(crate) world: String,
    /// Canonical build-input digest per Dockerfile-built image.
    pub(crate) image_inputs: BTreeMap<String, String>,
    /// Wall clock the daemon finished sealing this set.
    pub(crate) built_at_ms: u64,
    /// Sealed rootfs and runtime facts per image id (and per pulled resource reference).
    pub(crate) images: BTreeMap<String, ImageHead>,
}

impl WorldHeads {
    /// `<runtime_dir>/slipfork/heads/<world>.json`.
    pub(crate) fn path(runtime_dir: &Path, world: &WorldName) -> PathBuf {
        runtime_dir
            .join("slipfork")
            .join("heads")
            .join(format!("{}.json", world.as_str()))
    }

    /// Reads one world's heads, failing closed on a missing or unreadable file.
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|error| {
            format!(
                "cannot read image heads `{}`: {error}\nnext: run `slipfork up` to plan this world again",
                path.display()
            )
        })?;
        serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "cannot parse image heads `{}`: {error}\nnext: run `slipfork up` to rewrite the heads",
                path.display()
            )
        })
    }

    /// Writes the heads atomically (unique tmp + rename) so a reader never sees half a file.
    pub(crate) fn save(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("cannot encode image heads: {error}"))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create `{}`: {error}", parent.display()))?;
        }
        let temporary = unique_temp(path);
        fs::write(&temporary, bytes)
            .map_err(|error| format!("cannot write `{}`: {error}", temporary.display()))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("cannot install `{}`: {error}", path.display()))
    }

    /// Recomputes the build inputs and proves every planned rootfs is still a sealed subvolume.
    ///
    /// A stale `image_inputs` map, a vanished head, or a rootfs that is no longer a sealed Btrfs
    /// subvolume fails closed: `__supervise` never falls back to a build, and a fork never reuses
    /// a parent image whose checkout changed under it.
    pub(crate) fn validate(
        &self,
        project: &Path,
        doc: &Forkfile,
        world: &WorldName,
        plan: &WorldPlan,
        features: &FeatureSet,
        store_root: &Path,
    ) -> Result<(), String> {
        if self.v != HEADS_VERSION {
            return Err(format!(
                "image heads have schema v{}, expected v{HEADS_VERSION}\nnext: run `slipfork up` to rewrite the heads",
                self.v
            ));
        }
        if self.world != world.as_str() {
            return Err(format!(
                "image heads belong to world `{}`, not `{world}`\nnext: run `slipfork up` to plan this world again",
                self.world
            ));
        }
        let expected = crate::world::image_inputs(project, world, doc, &plan.images, features);
        if expected != self.image_inputs {
            return Err(
                "image build inputs changed since the heads were sealed\nnext: run `slipfork up` to rebuild the changed images"
                    .to_owned(),
            );
        }
        for node in &plan.images {
            if !self.images.contains_key(&node.id) {
                return Err(format!(
                    "no sealed image head for `{}`\nnext: run `slipfork up` to plan this world again",
                    node.id
                ));
            }
        }
        let engine = BtrfsEngine::new(StoreLayout::at(store_root.to_path_buf()));
        for (image, head) in &self.images {
            if !is_subvolume_path(&head.rootfs).unwrap_or(false)
                || !engine.is_sealed(&head.rootfs).unwrap_or(false)
            {
                return Err(format!(
                    "image `{image}` rootfs `{}` is gone or unsealed\nnext: run `slipfork up` to rebuild the image",
                    head.rootfs.display()
                ));
            }
        }
        Ok(())
    }
}

/// A per-attempt sibling so two writers never share the fixed `.tmp` name.
fn unique_temp(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    path.with_extension(format!("json.{}-{nanos}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use slipfork_forkfile::FeatureSet;
    use slipfork_world::{NodeKey, WorldName, world_plan_with};

    use super::{HEADS_VERSION, ImageHead, WorldHeads};

    fn temp(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    fn fixture() -> (
        tempfile::TempDir,
        slipfork_forkfile::ast::Forkfile,
        WorldName,
    ) {
        let root = temp("slipfork-heads");
        fs::write(
            root.path().join("Forkfile"),
            "# forkfile: 0.1\n\nimage web . {\n  dockerfile Dockerfile\n  arg PORT = web.port\n  service web {\n    port 3000\n    route /\n    run: sleep 3600\n  }\n}\n",
        )
        .expect("write Forkfile");
        fs::write(
            root.path().join("Dockerfile"),
            "FROM scratch\nENV PORT=8080\n",
        )
        .expect("write Dockerfile");
        fs::write(root.path().join("seed.txt"), "seed").expect("write context");
        let checked = slipfork_forkfile::compile(
            &fs::read_to_string(root.path().join("Forkfile")).expect("read Forkfile"),
        )
        .expect("compile Forkfile");
        let doc = *checked.document;
        let world = WorldName::parse("demo").expect("valid world name");
        (root, doc, world)
    }

    fn heads(
        world: &WorldName,
        image_inputs: BTreeMap<String, String>,
        rootfs: &str,
    ) -> WorldHeads {
        WorldHeads {
            v: HEADS_VERSION,
            world: world.as_str().to_owned(),
            image_inputs,
            built_at_ms: 1,
            images: BTreeMap::from([(
                "web".to_owned(),
                ImageHead {
                    key: NodeKey::from_digest([7; 32]),
                    rootfs: rootfs.into(),
                    inherited_env: vec!["PORT=8080".to_owned()],
                    argv: Vec::new(),
                    workdir: "/".to_owned(),
                    state: "built".to_owned(),
                },
            )]),
        }
    }

    /// A written file round-trips byte-for-byte through the atomic writer.
    #[test]
    fn heads_round_trip_through_the_atomic_file() {
        let (root, _doc, world) = fixture();
        let path = WorldHeads::path(root.path().join("run").as_path(), &world);
        let written = heads(
            &world,
            BTreeMap::new(),
            root.path().join("img").to_str().unwrap(),
        );
        written.save(&path).expect("write heads");
        assert_eq!(WorldHeads::load(&path).expect("read heads"), written);
        let parent = path.parent().expect("heads path has a parent");
        let leftovers: Vec<String> = fs::read_dir(parent)
            .expect("read the heads directory")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "an atomic save must leave no `*.tmp` behind: {leftovers:?}"
        );
    }

    /// A changed build input or a vanished/unsealed rootfs is refused, never rebuilt silently.
    #[test]
    fn validate_rejects_stale_inputs_and_missing_rootfs() {
        let (root, doc, world) = fixture();
        let features: FeatureSet = doc.default_features();
        let plan = world_plan_with(&doc, root.path(), &features).expect("world plan");
        let actual = crate::world::image_inputs(root.path(), &world, &doc, &plan.images, &features);

        let mut stale = heads(
            &world,
            BTreeMap::new(),
            root.path().join("img").to_str().unwrap(),
        );
        stale.image_inputs = BTreeMap::from([("web".to_owned(), "stale".to_owned())]);
        let error = stale
            .validate(root.path(), &doc, &world, &plan, &features, root.path())
            .expect_err("stale inputs must fail closed");
        assert!(error.starts_with("image build inputs changed"), "{error}");
        assert!(error.contains("\nnext: "), "one line + next: {error}");

        let mut versioned = heads(
            &world,
            actual.clone(),
            root.path().join("img").to_str().unwrap(),
        );
        versioned.v = HEADS_VERSION + 1;
        let error = versioned
            .validate(root.path(), &doc, &world, &plan, &features, root.path())
            .expect_err("an unknown schema version must fail closed");
        assert!(error.starts_with("image heads have schema v2"), "{error}");
        assert!(error.contains("\nnext: "), "one line + next: {error}");

        let mut foreign = heads(
            &world,
            actual.clone(),
            root.path().join("img").to_str().unwrap(),
        );
        foreign.world = "other".to_owned();
        let error = foreign
            .validate(root.path(), &doc, &world, &plan, &features, root.path())
            .expect_err("heads from another world must fail closed");
        assert!(error.contains("belong to world `other`"), "{error}");
        assert!(error.contains("\nnext: "), "one line + next: {error}");

        let missing = heads(
            &world,
            actual,
            root.path().join("vanished").to_str().unwrap(),
        );
        let error = missing
            .validate(root.path(), &doc, &world, &plan, &features, root.path())
            .expect_err("a vanished rootfs must fail closed");
        assert!(error.contains("gone or unsealed"), "{error}");
    }

    /// A resolved `arg` is a build input: heads sealed for port 3000 refuse a port-5173 doc, while
    /// heads carrying the port-5173 inputs pass the inputs check.
    #[test]
    fn validate_refuses_heads_whose_image_arg_changed() {
        let (root, doc3000, world) = fixture();
        let features3000 = doc3000.default_features();
        let plan3000 =
            world_plan_with(&doc3000, root.path(), &features3000).expect("port-3000 plan");
        let inputs3000 = crate::world::image_inputs(
            root.path(),
            &world,
            &doc3000,
            &plan3000.images,
            &features3000,
        );

        let source = fs::read_to_string(root.path().join("Forkfile")).expect("read Forkfile");
        let checked = slipfork_forkfile::compile(&source.replace("port 3000", "port 5173"))
            .expect("the port-5173 Forkfile compiles");
        let doc5173 = *checked.document;
        let features5173 = doc5173.default_features();
        let plan5173 =
            world_plan_with(&doc5173, root.path(), &features5173).expect("port-5173 plan");
        let inputs5173 = crate::world::image_inputs(
            root.path(),
            &world,
            &doc5173,
            &plan5173.images,
            &features5173,
        );
        assert_ne!(
            inputs3000, inputs5173,
            "a changed `arg PORT` must move the digest without any file edit"
        );

        let sealed3000 = heads(
            &world,
            inputs3000,
            root.path().join("img").to_str().unwrap(),
        );
        let error = sealed3000
            .validate(
                root.path(),
                &doc5173,
                &world,
                &plan5173,
                &features5173,
                root.path(),
            )
            .expect_err("heads sealed for port 3000 must refuse the port-5173 doc");
        assert!(
            error.starts_with("image build inputs changed since the heads were sealed"),
            "{error}"
        );
        assert!(error.contains("\nnext: "), "one line + next: {error}");

        let sealed5173 = heads(
            &world,
            inputs5173,
            root.path().join("img").to_str().unwrap(),
        );
        let error = sealed5173
            .validate(
                root.path(),
                &doc5173,
                &world,
                &plan5173,
                &features5173,
                root.path(),
            )
            .expect_err("the fixture has no sealed rootfs subvolume");
        assert!(
            !error.contains("build inputs changed"),
            "matching inputs must pass the inputs check: {error}"
        );
    }
}
