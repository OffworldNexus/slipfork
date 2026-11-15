//! Sole `[world]` world-image selector (`ADR-0009` §Decision 1).
//!
//! `[world]` accepts **at most two optional keys** and no precedence ladder:
//! `world.image` (which `snow.yml` `images[].id` builds the `L0` world rootfs) and `world.stage`
//! (which named `FROM ... AS <stage>` Slipfork extends into its `slipfork-world` stage). Every
//! other key — curated language bases, alternate root filesystem sources, `rootfs`/`dockerfile`
//! overrides — was rejected in `ADR-0009` and fails closed here with
//! [`WorldConfigError::UnknownKey`].
//!
//! # Resolution is pure and two-phase
//!
//! `slipfork-dag` never reads `snow.yml` or a `Dockerfile` (zero ambient I/O, zero `snow.yml`
//! dependency): the Composition Root (`slipforkd`) passes the plain id/stage slices it already
//! parsed. Resolution is deliberately two-phase because `world.image` selects the `BuildImage`,
//! and only that `BuildImage`'s `Dockerfile` declares the named stages:
//!
//! 1. [`WorldImageConfig::select_image`] validates `world.image` against `snow.yml`'s
//!    `images[].id` list: inferred when exactly one `BuildImage` exists, mandatory (failing closed
//!    with the candidate list) when two or more exist, and fail-closed when none is buildable.
//! 2. [`WorldImageConfig::select_stage`] validates `world.stage` against the selected
//!    `Dockerfile`'s named stages, defaulting to the final stage's name when it has one.

use thiserror::Error;

/// The `ADR-0009` §4.2 rule #1 prefix shared by every target-stage diagnostic.
const CONTRACT_RULE_1: &str = "World Image Contract #1 violated:";

/// The sole `[world]` schema: exactly two optional keys, unknown keys fail closed, zero fallbacks.
///
/// `WorldImageConfig::from_toml` is the only parse path (typed [`WorldConfigError`] diagnostics),
/// so an unknown key can never be silently accepted. Both keys are optional in TOML because a
/// single-`BuildImage` project with a named final `Dockerfile` stage needs no `[world]` table at
/// all; [`Self::select_image`] and [`Self::select_stage`] then infer the only possible selection or
/// fail closed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WorldImageConfig {
    /// `snow.yml` `images[].id` that seeds the `L0` world rootfs.
    ///
    /// Mandatory when `snow.yml` declares two or more `BuildImage` entries; inferred when it
    /// declares exactly one.
    pub image: Option<String>,
    /// Named `FROM ... AS <stage>` of the selected `BuildImage`'s `Dockerfile`.
    ///
    /// Defaults to the final stage's name when that stage is named (e.g. `stage = "builder"`
    /// keeps `cargo`/`rustc` available for hooks and `slipfork test` in compiled projects).
    pub stage: Option<String>,
}

impl WorldImageConfig {
    /// Validate `[world]` from its raw TOML value with typed diagnostics.
    ///
    /// `slipfork.toml` is parsed through this function rather than through `serde` directly so
    /// that an unknown key or a non-string value surfaces as a typed [`WorldConfigError`] naming
    /// the offending key instead of an opaque TOML decode string.
    pub(crate) fn from_toml(value: Option<&toml::Value>) -> Result<Self, WorldConfigError> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let toml::Value::Table(table) = value else {
            return Err(WorldConfigError::NotATable {
                found: value.type_str(),
            });
        };
        let mut config = Self::default();
        for (key, value) in table {
            match key.as_str() {
                "image" => config.image = Some(string_value("image", value)?),
                "stage" => config.stage = Some(string_value("stage", value)?),
                other => {
                    return Err(WorldConfigError::UnknownKey {
                        key: other.to_owned(),
                    });
                }
            }
        }
        Ok(config)
    }

    /// Phase 1: select the `snow.yml` `BuildImage` id (`world.image`).
    ///
    /// `build_image_ids` is the `id` of every buildable `snow.yml` `images[]` entry (a
    /// `BuildImage` carrying `context` + `dockerfile`), in declaration order. An explicit
    /// `world.image` must name one of them; without it a single `BuildImage` is inferred and two
    /// or more fail closed. A project with no buildable image fails closed with the four-line
    /// `images:` snippet that fixes it.
    pub fn select_image<'a>(
        &self,
        build_image_ids: &'a [String],
    ) -> Result<&'a str, WorldConfigError> {
        if build_image_ids.is_empty() {
            return Err(WorldConfigError::NoBuildableImage);
        }
        let Some(image) = &self.image else {
            return match build_image_ids {
                [only] => Ok(only),
                declared => Err(WorldConfigError::AmbiguousWorldImage {
                    declared: declared.to_vec(),
                }),
            };
        };
        if image.trim().is_empty() {
            return Err(WorldConfigError::EmptyWorldImage);
        }
        exact_match(image, build_image_ids, |image, declared| {
            WorldConfigError::UnknownWorldImage { image, declared }
        })
    }

    /// Phase 2: select the named `Dockerfile` stage (`world.stage`).
    ///
    /// `named_stages` is every `FROM ... AS <name>` stage of the selected `BuildImage`'s
    /// `Dockerfile`, in declaration order; `final_stage` is that `Dockerfile`'s final stage name
    /// (`None` when its final `FROM` has no `AS <name>`) and is always one of `named_stages` when
    /// present. An explicit `world.stage` must name one of them; without it the final stage's name
    /// is used and an unnamed final stage fails closed with the one-line `Dockerfile` fix.
    pub fn select_stage<'a>(
        &self,
        named_stages: &'a [String],
        final_stage: Option<&'a str>,
    ) -> Result<&'a str, WorldConfigError> {
        let Some(stage) = &self.stage else {
            return final_stage.ok_or_else(|| WorldConfigError::AmbiguousWorldStage {
                named: named_stages.to_vec(),
            });
        };
        if stage.trim().is_empty() {
            return Err(WorldConfigError::EmptyWorldStage);
        }
        exact_match(stage, named_stages, |stage, declared| {
            WorldConfigError::UnknownWorldStage { stage, declared }
        })
    }
}

/// Typed `[world]` failures (`ADR-0009` §5), each carrying its exact actionable fix.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum WorldConfigError {
    /// `world` was declared as something other than a TOML table.
    #[error("`[world]` must be a TOML table with optional `image` / `stage` keys, found {found}")]
    NotATable { found: &'static str },
    /// `[world]` declared a key outside `image` / `stage`.
    #[error("unknown `[world]` key `{key}`; `[world]` accepts only `image` and `stage`")]
    UnknownKey { key: String },
    /// `world.image` / `world.stage` was declared with a non-string TOML value.
    #[error("`world.{key}` must be a string, found {found}")]
    NonStringValue {
        key: &'static str,
        found: &'static str,
    },
    /// `snow.yml` declares no `BuildImage` entry, so no world rootfs can be built.
    #[error(
        "snow.yml declares no buildable Dockerfile under 'images:'; a project that cannot be built into an image is not a project Slipfork can fork\n  fix: add a named Dockerfile stage and declare it in snow.yml:\n    images:\n      - id: app\n        context: .\n        dockerfile: Dockerfile"
    )]
    NoBuildableImage,
    /// `snow.yml` declares two or more `BuildImage` entries and `world.image` does not select one.
    #[error(
        "snow.yml declares {} buildable images ({}) and slipfork.toml does not select one\n  fix: add `world.image = \"{}\"` under `[world]` in slipfork.toml",
        declared.len(),
        quoted(declared),
        declared.first().map_or("", String::as_str)
    )]
    AmbiguousWorldImage { declared: Vec<String> },
    /// `world.image` names no `BuildImage` declared by `snow.yml`.
    #[error(
        "slipfork.toml sets `world.image = \"{image}\"`, which matches no buildable image in snow.yml (available: {})\n  fix: set `world.image = \"{}\"` in slipfork.toml",
        quoted(declared),
        declared.first().map_or("", String::as_str)
    )]
    UnknownWorldImage {
        image: String,
        declared: Vec<String>,
    },
    /// `world.image` is an empty or whitespace-only string.
    #[error("`world.image` must name a `snow.yml` image id, not an empty string")]
    EmptyWorldImage,
    /// `world.stage` is an empty or whitespace-only string.
    #[error("`world.stage` must name a `FROM ... AS <stage>` stage, not an empty string")]
    EmptyWorldStage,
    /// `world.stage` names a stage the selected `Dockerfile` does not declare.
    #[error(
        "{} `world.stage = \"{stage}\"` not found in the selected Dockerfile (declared stages: {})\n  fix: {}",
        CONTRACT_RULE_1,
        quoted(declared),
        stage_fix(declared)
    )]
    UnknownWorldStage {
        stage: String,
        declared: Vec<String>,
    },
    /// No `world.stage` and the `Dockerfile`'s final stage has no `AS <name>` to target.
    #[error(
        "{} the selected Dockerfile's final stage has no `AS <name>` and slipfork.toml sets no `world.stage`\n  fix: add `AS app` to the final `FROM` line{}",
        CONTRACT_RULE_1,
        final_stage_fix(named)
    )]
    AmbiguousWorldStage { named: Vec<String> },
}

fn string_value(key: &'static str, value: &toml::Value) -> Result<String, WorldConfigError> {
    match value {
        toml::Value::String(value) => Ok(value.clone()),
        other => Err(WorldConfigError::NonStringValue {
            key,
            found: other.type_str(),
        }),
    }
}

/// Match an explicit `world.image` / `world.stage` against the ids/stages the caller resolved.
fn exact_match<'a>(
    selector: &str,
    available: &'a [String],
    unknown: impl FnOnce(String, Vec<String>) -> WorldConfigError,
) -> Result<&'a str, WorldConfigError> {
    available
        .iter()
        .find(|candidate| *candidate == selector)
        .map(String::as_str)
        .ok_or_else(|| unknown(selector.to_owned(), available.to_vec()))
}

/// Render declared ids/stages as `'api', 'worker'` for actionable diagnostics.
fn quoted(items: &[String]) -> String {
    if items.is_empty() {
        return "none".to_owned();
    }
    items
        .iter()
        .map(|item| format!("'{item}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn stage_fix(declared: &[String]) -> String {
    if declared.is_empty() {
        "declare a named `FROM ... AS <stage>` in the Dockerfile".to_owned()
    } else {
        format!(
            "set `world.stage` in slipfork.toml to one of: {}",
            quoted(declared)
        )
    }
}

fn final_stage_fix(named: &[String]) -> String {
    match named.last() {
        Some(last) => format!(" (or set `world.stage = \"{last}\"` in slipfork.toml)"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{WorldConfigError, WorldImageConfig};

    fn parse(raw: &str) -> Result<WorldImageConfig, WorldConfigError> {
        let value: toml::Value = toml::from_str(raw).expect("valid TOML");
        let world = value.get("world");
        WorldImageConfig::from_toml(world)
    }

    #[test]
    fn non_table_world_fails_closed() {
        assert_eq!(
            parse("world = 7").expect_err("scalar [world]"),
            WorldConfigError::NotATable { found: "integer" }
        );
    }
}
