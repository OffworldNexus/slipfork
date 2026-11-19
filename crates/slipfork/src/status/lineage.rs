//! The single human lineage line body (`docs/design/dx-0.1.0-ux.md` §8.2).

use slipfork_world::{CacheOutcome, SeedLineage, WorldLineage};

use crate::ui::TerminalUi;

/// The single human lineage line body (`docs/design/dx-0.1.0-ux.md` §8.2).
///
/// Order is `rootfs`, one entry per resolved service image, `seed`, `base`; a fact the daemon never
/// recorded is `-`, and only the four outcome words appear (`cached`, `rebuilt <dur>`, `reused`,
/// `sealed <dur>`) — never the wire `hit`/`miss`. This is the one renderer of that line: `status`
/// and the `up` / `fork` / `restart` block ([`crate::restart`]) both call it, so the compact line
/// can never drift between the fleet view and the actionable block.
pub(crate) fn render_lineage(lineage: &WorldLineage, ui: &TerminalUi) -> String {
    let unknown = || ui.secondary("-");
    let mut parts = vec![match &lineage.rootfs {
        Some(rootfs) => format!(
            "rootfs {} {}",
            ui.digest(&rootfs.digest),
            cache_outcome(rootfs.cache, rootfs.millis)
        ),
        None => format!("rootfs {} {}", unknown(), unknown()),
    }];
    for service in &lineage.services {
        parts.push(format!(
            "{} {}",
            service.id,
            cache_outcome(service.cache, service.millis)
        ));
    }
    parts.push(match &lineage.seed {
        Some(seed) => format!(
            "seed {} {}",
            seed.digest
                .as_deref()
                .map_or_else(unknown, |digest| ui.digest(digest)),
            seed_outcome(seed)
        ),
        None => format!("seed {} {}", unknown(), unknown()),
    });
    parts.push(match &lineage.base {
        Some(base) => format!(
            "base {} {}",
            ui.digest(&base.key),
            base_outcome(base.millis)
        ),
        None => format!("base {} {}", unknown(), unknown()),
    });
    parts.join(" · ")
}

/// The unknown-fact placeholder: never `0`, never an empty string.
fn unknown() -> String {
    "-".to_owned()
}

/// Rootfs/service-image outcome: `cached` for a hit, `rebuilt <dur>` for a miss.
fn cache_outcome(cache: CacheOutcome, millis: Option<u64>) -> String {
    match cache {
        CacheOutcome::Hit => "cached".to_owned(),
        CacheOutcome::Miss => format!("rebuilt {}", human_duration(millis)),
    }
}

/// Seed outcome: `cached` when the base was reused, `rebuilt <dur>` when it was restored.
fn seed_outcome(seed: &SeedLineage) -> String {
    match seed.rebuilt {
        Some(false) => "cached".to_owned(),
        Some(true) => format!("rebuilt {}", human_duration(seed.millis)),
        None => unknown(),
    }
}

/// Base outcome: `reused` when the sealed snapshot already existed, `sealed <dur>` otherwise.
fn base_outcome(millis: Option<u64>) -> String {
    match millis {
        None => "reused".to_owned(),
        Some(_) => format!("sealed {}", human_duration(millis)),
    }
}

/// A measured duration, or `-` when the daemon recorded none.
fn human_duration(millis: Option<u64>) -> String {
    match millis {
        None => unknown(),
        Some(millis) if millis < 1_000 => format!("{millis}ms"),
        Some(millis) => format!("{:.1}s", millis as f64 / 1_000.0),
    }
}

#[cfg(test)]
mod tests {
    use slipfork_world::{
        BaseLineage, RootfsLineage, SeedLineage, ServiceImageLineage, WorldLineage,
    };

    use super::*;
    use crate::ui::Capabilities;

    fn plain() -> TerminalUi {
        TerminalUi::new(Capabilities {
            tty: false,
            color: false,
            verbose: false,
            json: false,
        })
    }

    fn digest(prefix: &str) -> String {
        format!("{prefix}{}", "0".repeat(64 - prefix.len()))
    }

    #[test]
    fn lineage_prints_short_digests_and_a_dash_for_an_unknown_fact() {
        assert_eq!(
            render_lineage(
                &WorldLineage {
                    rootfs: None,
                    services: Vec::new(),
                    seed: None,
                    base: None,
                },
                &plain(),
            ),
            "rootfs - - · seed - - · base - -"
        );

        let full = WorldLineage {
            rootfs: Some(RootfsLineage {
                digest: digest("a1b2c3d4e5f6"),
                cache: CacheOutcome::Miss,
                millis: Some(2_104),
            }),
            services: vec![ServiceImageLineage {
                id: "postgres:18".to_owned(),
                cache: CacheOutcome::Hit,
                millis: None,
            }],
            seed: Some(SeedLineage {
                digest: Some(digest("9f8e7d6c5b4a")),
                rebuilt: Some(true),
                millis: Some(10),
            }),
            base: Some(BaseLineage {
                key: digest("4570132bffb8"),
                millis: Some(10),
            }),
        };
        assert_eq!(
            render_lineage(&full, &plain()),
            "rootfs a1b2c3d4e5f6 rebuilt 2.1s · postgres:18 cached · seed 9f8e7d6c5b4a rebuilt 10ms · base 4570132bffb8 sealed 10ms"
        );

        // An unrecorded verdict stays `-` instead of inventing a rebuild.
        let unmeasured = WorldLineage {
            seed: Some(SeedLineage {
                digest: Some(digest("aa")),
                rebuilt: Some(true),
                millis: None,
            }),
            ..full
        };
        assert!(render_lineage(&unmeasured, &plain()).contains("seed aa0000000000 rebuilt -"));
    }
}
