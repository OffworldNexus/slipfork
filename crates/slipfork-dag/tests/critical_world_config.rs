//! Protected critical tests for the sole `[world]` schema (`ADR-0009` §Decision 1).
//!
//! `[world]` is a selector, never a menu: exactly `world.image` + `world.stage`, both optional,
//! `deny_unknown_fields`, zero fallbacks and zero precedence ladder. `slipfork-dag` never parses
//! `snow.yml` or a `Dockerfile`, so the Composition Root passes the plain id/stage slices it already
//! owns — every test below drives resolution through those plain slices.

use slipfork_dag::{DagError, SlipforkConfig, WorldConfigError, WorldImageConfig};

/// The `snow.yml` `images[].id` list the Composition Root resolved for the project.
fn build_images(ids: &[&str]) -> Vec<String> {
    ids.iter().copied().map(str::to_owned).collect()
}

/// The named `FROM ... AS <name>` stages of the selected `Dockerfile`, in declaration order.
fn named_stages(names: &[&str]) -> Vec<String> {
    names.iter().copied().map(str::to_owned).collect()
}

fn parse(raw: &str) -> SlipforkConfig {
    SlipforkConfig::parse_toml(raw).expect("valid slipfork.toml")
}

#[test]
fn single_build_image_needs_zero_world_config() {
    // `[world]` omitted entirely: the only `BuildImage` and its named final stage are inferred.
    let cfg = parse("");
    assert_eq!(cfg.world, WorldImageConfig::default());

    let images = build_images(&["api"]);
    assert_eq!(
        cfg.world.select_image(&images).expect("single image"),
        "api"
    );
    let stages = named_stages(&["api"]);
    assert_eq!(
        cfg.world
            .select_stage(&stages, Some("api"))
            .expect("named final stage"),
        "api"
    );
}

#[test]
fn schema_accepts_only_world_image_and_world_stage() {
    let cfg = parse("[world]\nimage = \"api\"\nstage = \"builder\"\n");
    assert_eq!(cfg.world.image.as_deref(), Some("api"));
    assert_eq!(cfg.world.stage.as_deref(), Some("builder"));
}

#[test]
fn two_build_images_without_world_image_fail_closed() {
    let cfg = parse("");
    let images = build_images(&["api", "worker"]);
    let error = cfg
        .world
        .select_image(&images)
        .expect_err("two buildable images require world.image");
    assert_eq!(
        error,
        WorldConfigError::AmbiguousWorldImage {
            declared: build_images(&["api", "worker"]),
        }
    );
    let message = error.to_string();
    assert!(
        message.contains("2 buildable images ('api', 'worker')"),
        "names the candidates: {message}"
    );
    assert!(
        message.contains("`world.image = \"api\"`"),
        "names the key to add with an actionable default: {message}"
    );
}

#[test]
fn world_image_selects_among_multiple_build_images() {
    let cfg = parse("[world]\nimage = \"worker\"\n");
    let images = build_images(&["api", "worker"]);
    assert_eq!(
        cfg.world.select_image(&images).expect("declared image"),
        "worker"
    );
}

#[test]
fn undeclared_world_image_fails_closed() {
    let cfg = parse("[world]\nimage = \"backend\"\n");
    let images = build_images(&["api", "worker"]);
    let error = cfg
        .world
        .select_image(&images)
        .expect_err("world.image must name a BuildImage id");
    assert_eq!(
        error,
        WorldConfigError::UnknownWorldImage {
            image: "backend".to_owned(),
            declared: images.clone(),
        }
    );
    let message = error.to_string();
    assert!(
        message.contains("`world.image = \"backend\"`"),
        "names the rejected value: {message}"
    );
    assert!(
        message.contains("available: 'api', 'worker'"),
        "names every buildable image: {message}"
    );
    assert!(
        message.contains("`world.image = \"api\"`"),
        "names the fix: {message}"
    );
}

#[test]
fn no_buildable_image_fails_closed_with_the_images_snippet() {
    let cfg = parse("");
    let error = cfg
        .world
        .select_image(&[])
        .expect_err("zero buildable images cannot seed a world rootfs");
    assert_eq!(error, WorldConfigError::NoBuildableImage);
    let message = error.to_string();
    for line in [
        "images:",
        "- id: app",
        "context: .",
        "dockerfile: Dockerfile",
    ] {
        assert!(
            message.contains(line),
            "four-line snippet line {line:?}: {message}"
        );
    }
    assert!(
        message.contains("cannot be built into an image is not a project Slipfork can fork"),
        "states the project-level consequence: {message}"
    );
}

#[test]
fn world_stage_targets_a_named_dockerfile_stage() {
    // Single `BuildImage` (inferred) plus an explicit `world.stage`: the compiled-project workflow.
    let cfg = parse("[world]\nstage = \"builder\"\n");
    let images = build_images(&["api"]);
    assert_eq!(
        cfg.world.select_image(&images).expect("single image"),
        "api"
    );
    let stages = named_stages(&["builder", "runtime"]);
    assert_eq!(
        cfg.world
            .select_stage(&stages, Some("runtime"))
            .expect("explicit builder stage wins over the final stage"),
        "builder"
    );
}

#[test]
fn both_world_keys_resolve_together() {
    let cfg = parse("[world]\nimage = \"api\"\nstage = \"builder\"\n");
    let images = build_images(&["api", "worker"]);
    let stages = named_stages(&["builder", "runtime"]);
    assert_eq!(
        cfg.world.select_image(&images).expect("declared image"),
        "api"
    );
    assert_eq!(
        cfg.world
            .select_stage(&stages, Some("runtime"))
            .expect("declared stage"),
        "builder"
    );
}

#[test]
fn unknown_world_stage_fails_closed() {
    let cfg = parse("[world]\nstage = \"dev\"\n");
    let stages = named_stages(&["builder", "runtime"]);
    let error = cfg
        .world
        .select_stage(&stages, Some("runtime"))
        .expect_err("world.stage must name a declared stage");
    assert_eq!(
        error,
        WorldConfigError::UnknownWorldStage {
            stage: "dev".to_owned(),
            declared: stages.clone(),
        }
    );
    let message = error.to_string();
    assert!(
        message.contains("`world.stage = \"dev\"` not found"),
        "names the rejected stage: {message}"
    );
    assert!(
        message.contains("declared stages: 'builder', 'runtime'"),
        "lists the declared stages: {message}"
    );
}

#[test]
fn unnamed_final_stage_without_world_stage_fails_closed() {
    let cfg = parse("");
    // `FROM builder` ... `FROM debian:trixie-slim` (unnamed): no single targetable stage name.
    let stages = named_stages(&["builder"]);
    let error = cfg
        .world
        .select_stage(&stages, None)
        .expect_err("an unnamed final stage cannot be targeted");
    assert_eq!(
        error,
        WorldConfigError::AmbiguousWorldStage {
            named: stages.clone(),
        }
    );
    let message = error.to_string();
    assert!(
        message.contains("no `AS <name>`"),
        "names the missing stage name: {message}"
    );
    assert!(
        message.contains("`world.stage = \"builder\"`"),
        "points at the one-line slipfork.toml fix: {message}"
    );
}

#[test]
fn unknown_world_keys_fail_closed() {
    // Curated language bases, alternate root filesystem sources, and extra image modes were
    // rejected by `ADR-0009`; they are hard parse errors, never silently ignored.
    for key in ["base", "rootfs", "dockerfile"] {
        let raw = format!("[world]\n{key} = \"x\"\n");
        let error = SlipforkConfig::parse_toml(&raw).expect_err("unknown [world] key");
        let DagError::WorldConfig(WorldConfigError::UnknownKey { key: found }) = &error else {
            panic!("unknown [world] key `{key}` must be a typed WorldConfigError: {error:?}");
        };
        assert_eq!(found, key);
        assert!(
            error
                .to_string()
                .contains("`[world]` accepts only `image` and `stage`"),
            "error must name the two accepted keys: {error}"
        );
    }
}

#[test]
fn non_string_world_values_fail_closed() {
    let error = SlipforkConfig::parse_toml("[world]\nimage = 7\n")
        .expect_err("non-string world.image must fail closed");
    let DagError::WorldConfig(WorldConfigError::NonStringValue { key, found }) = &error else {
        panic!("non-string world.image must be a typed WorldConfigError: {error:?}");
    };
    assert_eq!((*key, *found), ("image", "integer"));

    let error = SlipforkConfig::parse_toml("[world]\n[world.stage]\nname = \"api\"\n")
        .expect_err("a nested table is not a stage string");
    let DagError::WorldConfig(WorldConfigError::NonStringValue { key, found }) = &error else {
        panic!("nested [world.stage] must be a non-string value error: {error:?}");
    };
    assert_eq!((*key, *found), ("stage", "table"));
}

#[test]
fn empty_world_values_fail_closed() {
    let cfg = parse("[world]\nimage = \"  \"\n");
    let images = build_images(&["api"]);
    assert_eq!(
        cfg.world
            .select_image(&images)
            .expect_err("blank world.image"),
        WorldConfigError::EmptyWorldImage
    );

    let cfg = parse("[world]\nstage = \"\"\n");
    let stages = named_stages(&["api"]);
    assert_eq!(
        cfg.world
            .select_stage(&stages, Some("api"))
            .expect_err("blank world.stage"),
        WorldConfigError::EmptyWorldStage
    );
}
