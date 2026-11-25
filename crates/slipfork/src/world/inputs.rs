//! The canonical build-input digest and resolved build args one world's images share.
//!
//! `slipfork-image` owns the `.dockerignore` walk and the digest; every caller (the daemon's
//! staleness check and the world stamp's epoch) folds this same map, so a `Dockerfile`, a resolved
//! `arg` (a service port or domain), or a copied context edit always rewrites the key and rebuilds.

use std::collections::BTreeMap;
use std::path::Path;

use slipfork_forkfile::FeatureSet;
use slipfork_forkfile::ast::Forkfile;
use slipfork_image::BuildArgs;
use slipfork_world::{ImageNode, WorldName};

/// Production ingress scheme every resolved `arg` URL uses; `.localhost` is plain HTTP.
pub(crate) const INGRESS_SCHEME: &str = "http";

/// The resolved build-time `arg` values for `image_id`, ready for the builder and the digest.
pub(crate) fn build_args(
    doc: &Forkfile,
    world: &str,
    scheme: &str,
    image_id: &str,
    features: &FeatureSet,
) -> BuildArgs {
    let mut args = BuildArgs::new();
    for (name, value) in doc.eval_image_args_with(world, scheme, image_id, features) {
        args.declare(name, value);
    }
    args
}

/// The canonical build-input digest per build image (`Dockerfile` bytes + resolved `arg` values +
/// context hash).
///
/// `slipfork-image` owns the `.dockerignore` walk and the digest; every caller (the daemon's
/// staleness check and the world stamp's epoch) folds this same map so a `Dockerfile`, a resolved
/// `arg` (a service port or domain), or a copied context edit always rewrites the key and rebuilds.
pub(crate) fn image_inputs(
    project: &Path,
    world: &WorldName,
    doc: &Forkfile,
    images: &[ImageNode],
    features: &FeatureSet,
) -> BTreeMap<String, String> {
    images
        .iter()
        .filter(|image| image.from.is_none())
        .filter_map(|image| {
            let paths = image.paths(project);
            let target = doc.eval_image_stage_with(&image.id, features);
            let args = build_args(doc, world.as_str(), INGRESS_SCHEME, &image.id, features);
            slipfork_image::build_inputs_digest(
                &paths.context,
                &paths.dockerfile,
                target.as_deref(),
                &args,
            )
            .ok()
            .map(|digest| (image.id.clone(), digest))
        })
        .collect()
}
