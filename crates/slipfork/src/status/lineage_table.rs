//! The `-v` per-node `[lineage]` table (`docs/design/dx-0.1.0-ux.md` §8.2).
//!
//! One cohesive renderer over the daemon's [`WorldLineage`]: every column width is measured from
//! the visible cells (ANSI stripped), so a long service image widens the node column instead of
//! shearing the digest/outcome columns. The compact one-line lineage lives in [`super::lineage`].

use slipfork_world::{CacheOutcome, WorldLineage};

use crate::ui::{TerminalUi, column_width};

/// Narrowest node column (`rootfs      `, `postgres:18 `); a longer image id widens it.
const MIN_NODE_COLUMN: usize = 12;

/// Narrowest digest column (a `<digest12>` or `-`).
const MIN_DIGEST_COLUMN: usize = 14;

/// Narrowest outcome column (`cached`, `rebuilt`, ...).
const MIN_OUTCOME_COLUMN: usize = 13;

/// One node's verbose lineage fact, measured before the shared columns are rendered.
struct DetailRow<'a> {
    node: &'a str,
    digest: &'a str,
    outcome: &'a str,
    millis: Option<u64>,
}

/// Render the per-node `[lineage]` table: `rootfs`, every service image, `seed`, then `base`.
///
/// An unknown fact is `-`; the wire words `hit`/`miss` never appear.
pub(crate) fn render(lineage: &WorldLineage, ui: &TerminalUi) -> String {
    let mut rows = Vec::new();
    rows.push(match &lineage.rootfs {
        Some(rootfs) => DetailRow {
            node: "rootfs",
            digest: &rootfs.digest,
            outcome: cache_word(rootfs.cache),
            millis: rootfs.millis,
        },
        None => DetailRow {
            node: "rootfs",
            digest: "-",
            outcome: "-",
            millis: None,
        },
    });
    for service in &lineage.services {
        // A service-image row records no digest; only its cache verdict and duration.
        rows.push(DetailRow {
            node: &service.id,
            digest: "-",
            outcome: cache_word(service.cache),
            millis: service.millis,
        });
    }
    rows.push(match &lineage.seed {
        Some(seed) => DetailRow {
            node: "seed",
            digest: seed.digest.as_deref().unwrap_or("-"),
            outcome: seed_word(seed.rebuilt),
            millis: seed.millis,
        },
        None => DetailRow {
            node: "seed",
            digest: "-",
            outcome: "-",
            millis: None,
        },
    });
    rows.push(match &lineage.base {
        Some(base) => DetailRow {
            node: "base",
            digest: &base.key,
            outcome: if base.millis.is_none() {
                "reused"
            } else {
                "sealed"
            },
            millis: base.millis,
        },
        None => DetailRow {
            node: "base",
            digest: "-",
            outcome: "-",
            millis: None,
        },
    });

    let node_width = column_width(MIN_NODE_COLUMN, rows.iter().map(|row| row.node));
    let outcome_width = column_width(MIN_OUTCOME_COLUMN, rows.iter().map(|row| row.outcome));
    let painted: Vec<String> = rows
        .iter()
        .map(|row| painted_digest(row.digest, ui))
        .collect();
    let digest_width = column_width(MIN_DIGEST_COLUMN, painted.iter().map(String::as_str));

    let mut out = String::new();
    for (row, digest) in rows.iter().zip(&painted) {
        let duration = row
            .millis
            .map_or_else(|| "-".to_owned(), |millis| format!("{millis}ms"));
        out.push_str(&format!(
            "[lineage]   {} {} {} {duration}\n",
            ui.pad(row.node, node_width),
            ui.pad(digest, digest_width),
            ui.pad(row.outcome, outcome_width),
        ));
    }
    out
}

/// One digest cell: the 12-char dimmed digest, or a dimmed dash for an unrecorded fact.
fn painted_digest(digest: &str, ui: &TerminalUi) -> String {
    if digest == "-" {
        ui.secondary("-")
    } else {
        ui.digest(digest)
    }
}

/// Rootfs/service-image outcome: the human word for the daemon's cache verdict.
fn cache_word(cache: CacheOutcome) -> &'static str {
    match cache {
        CacheOutcome::Hit => "cached",
        CacheOutcome::Miss => "rebuilt",
    }
}

/// Seed outcome: `cached` when the base was reused, `rebuilt` when it was restored, `-` unknown.
fn seed_word(rebuilt: Option<bool>) -> &'static str {
    match rebuilt {
        Some(false) => "cached",
        Some(true) => "rebuilt",
        None => "-",
    }
}
