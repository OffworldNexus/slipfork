//! The one actionable `up` / `fork` / `restart` block (`docs/design/dx-0.1.0-ux.md` §1, §4, §8.2).
//!
//! Every function here is pure over the single
//! [`WorldUpSummary`] the daemon returned: the CLI performs exactly
//! one boot (or restart) round trip and hands the typed summary to these renderers. Nothing is
//! recomputed from `snow.yml`, the process table, or the filesystem — `world`, `state`, `changed`,
//! `restarted`, the origin, the worktree, the lineage, and `next` are the daemon's own observed
//! facts, and a fact the daemon did not record renders `-`, never `0` and never an invented
//! duration.
//!
//! The compact lineage line belongs to `crate::status::render_lineage` (§8.2's one renderer);
//! this module only adds the block around it and the `-v` per-node table.

use serde_json::Value;
use slipfork_wire::control::{CacheOutcome, WorldLineage, WorldState, WorldUpSummary};

use crate::status::{render_lineage, short_digest};

/// Width of the block's label column (`world     `, `worktree  `, `lineage   `).
const LABEL_WIDTH: usize = 10;

/// Width of the verbose table's node column (`rootfs      `, `postgres:18 `).
const NODE_WIDTH: usize = 12;

/// Width of the verbose table's digest column (a `<digest12>` or `-`).
const DIGEST_WIDTH: usize = 14;

/// Width of the verbose table's outcome column (`cached`, `rebuilt`, ...).
const OUTCOME_WIDTH: usize = 13;

/// The three verbs that print the actionable block.
///
/// `restart` leads with its verdict line; `fork` adds the `cd` row; `up` alone carries the
/// "run `slipfork restart` to apply edits" hint on an unchanged world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// `slipfork up` (boot, rescue, or the idempotent confirm).
    Up,
    /// `slipfork fork <FORK>`.
    Fork,
    /// `slipfork restart`.
    Restart,
}

/// The two presentation facts the CLI knows and the daemon summary cannot carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Append `--world <world>` to every `next` command (the world is not `cwd`-selected).
    pub qualify_next: bool,
    /// Add the `[resources]` rows and the per-node `[lineage]` table (global `-v`).
    pub verbose: bool,
}

/// Render the whole human block: verdict, `world`, `app`, `worktree`, `cd`/verdict row, `next`,
/// the one compact `lineage` line, and — only under `-v` — the `[resources]` and `[lineage]` detail.
pub fn render(verb: Verb, summary: &WorldUpSummary, options: Options) -> String {
    let mut out = String::new();
    if verb == Verb::Restart {
        out.push_str(&verdict(summary));
        out.push('\n');
    }
    out.push_str(&row("world", &world_value(verb, summary)));
    out.push_str(&row("app", summary.origin.url()));
    out.push_str(&row("worktree", &summary.worktree.display().to_string()));
    if verb == Verb::Fork {
        out.push_str(&row("cd", &format!("cd {}", summary.worktree.display())));
    }
    out.push_str(&row("next", &next_line(summary, options.qualify_next)));
    out.push_str(&row("lineage", &render_lineage(&summary.lineage)));
    if options.verbose {
        for resource in &summary.resources {
            out.push_str(&format!(
                "[resources] {}: {} -> {} ({})\n",
                resource.id, resource.strategy, resource.resolved, resource.reason
            ));
        }
        out.push_str(&detail(&summary.lineage));
    }
    out
}

/// The `restart` verdict: the ids the daemon observed coming back, or the honest no-op.
///
/// `from the daemon's observed restarted ids, never recomputed here`. `changed` without any
/// restarted id means the run only re-ran hooks, so the line says `[changed]` instead of naming a
/// service that never came back.
fn verdict(summary: &WorldUpSummary) -> String {
    if !summary.changed {
        return "[no change]".to_owned();
    }
    if summary.restarted.is_empty() {
        return "[changed]".to_owned();
    }
    format!("[restarted] {}", summary.restarted.join(", "))
}

/// The `world` row value: the canonical id, its lifecycle state, and whether this run changed it.
fn world_value(verb: Verb, summary: &WorldUpSummary) -> String {
    let state = match summary.state {
        WorldState::Running => "running",
        WorldState::Failed => "failed",
        WorldState::Stopped => "stopped",
    };
    let changed = if summary.changed {
        "changed"
    } else if verb == Verb::Up {
        "unchanged — run `slipfork restart` to apply edits"
    } else {
        "unchanged"
    };
    format!("{} ({state}, {changed})", summary.world)
}

/// The `next` row: the daemon's one state table, optionally qualified for a non-`cwd` world.
fn next_line(summary: &WorldUpSummary, qualify: bool) -> String {
    summary
        .next
        .iter()
        .map(|command| {
            if qualify {
                format!("{command} --world {}", summary.world)
            } else {
                command.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// One `label     value` block row (`world     demo`, `lineage   rootfs ...`).
fn row(label: &str, value: &str) -> String {
    format!("{label:<LABEL_WIDTH$}{value}\n")
}

/// The `-v` per-node lineage table (`docs/design/dx-0.1.0-ux.md` §8.2).
///
/// One row per node in the same order as the compact line — `rootfs`, every service image, `seed`,
/// `base` — with the digest, the four-word human outcome, and the measured milliseconds. An
/// unknown fact is `-`; the wire words `hit`/`miss` never appear.
fn detail(lineage: &WorldLineage) -> String {
    let mut out = String::new();
    match &lineage.rootfs {
        Some(rootfs) => {
            out.push_str(&detail_row(
                "rootfs",
                &short_digest(&rootfs.digest),
                cache_word(rootfs.cache),
                rootfs.millis,
            ));
        }
        None => out.push_str(&detail_row("rootfs", "-", "-", None)),
    }
    for service in &lineage.services {
        // A service-image row records no digest; only its cache verdict and duration.
        out.push_str(&detail_row(
            &service.id,
            "-",
            cache_word(service.cache),
            service.millis,
        ));
    }
    match &lineage.seed {
        Some(seed) => out.push_str(&detail_row(
            "seed",
            seed.digest
                .as_deref()
                .map(short_digest)
                .as_deref()
                .unwrap_or("-"),
            seed_word(seed.rebuilt),
            seed.millis,
        )),
        None => out.push_str(&detail_row("seed", "-", "-", None)),
    }
    match &lineage.base {
        Some(base) => out.push_str(&detail_row(
            "base",
            &short_digest(&base.key),
            if base.millis.is_none() {
                "reused"
            } else {
                "sealed"
            },
            base.millis,
        )),
        None => out.push_str(&detail_row("base", "-", "-", None)),
    }
    out
}

/// One `[lineage]` detail row, columns aligned with the §8.2 transcript.
fn detail_row(node: &str, digest: &str, outcome: &str, millis: Option<u64>) -> String {
    let duration = millis
        .map(|millis| format!("{millis}ms"))
        .unwrap_or_else(|| "-".to_owned());
    format!(
        "[lineage]   {node:<NODE_WIDTH$} {digest:<DIGEST_WIDTH$} {outcome:<OUTCOME_WIDTH$} {duration}\n"
    )
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

/// The `up` / `fork` / `restart` `--json` document: the daemon's [`WorldUpSummary`] as-is, plus the
/// one `"ok": true` key every object document carries (`docs/design/dx-0.1.0-ux.md` §10).
///
/// One projection: a field this block does not render still travels, and no CLI-side value is
/// added beyond the envelope's `ok`.
pub fn json(summary: &WorldUpSummary) -> Result<Value, String> {
    let mut document = serde_json::to_value(summary)
        .map_err(|err| format!("cannot render the world document: {err}"))?;
    let Value::Object(ref mut map) = document else {
        return Err("the world document must be a JSON object".to_owned());
    };
    map.insert("ok".to_owned(), Value::Bool(true));
    // The document's `origin` is always the typed origin's one rendered URL (§3), never the
    // internally tagged wire enum; every other key is the daemon's summary field untouched.
    map.insert(
        "origin".to_owned(),
        Value::String(summary.origin.url().to_owned()),
    );
    Ok(document)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use slipfork_wire::control::{
        BaseLineage, ResourceSummary, RootfsLineage, SeedLineage, ServiceImageLineage,
        WorldServiceSummary,
    };

    use crate::status::world_next_commands;
    use slipfork_engine::{ServiceKind, ServiceState, WorldServiceInfo};
    use slipfork_world::{LocalHostRoute, ProjectSlug, WorldOrigin, WorldTarget};

    use super::*;

    fn origin(project: &str, fork: Option<&str>, ingress_port: u16) -> WorldOrigin {
        let project = ProjectSlug::parse(project).expect("a valid project slug");
        let target = match fork {
            Some(fork) => WorldTarget::UserFork(fork.to_owned()),
            None => WorldTarget::Canonical,
        };
        let route = LocalHostRoute::new(project, target, None, &[]).expect("a routable route");
        WorldOrigin::local(&route, ingress_port).expect("a minted origin")
    }

    fn digest(prefix: &str) -> String {
        format!("{prefix}{}", "0".repeat(64 - prefix.len()))
    }

    fn lineage(rootfs: Option<&str>, seed: Option<&str>, base: Option<&str>) -> WorldLineage {
        WorldLineage {
            rootfs: rootfs.map(|digest| RootfsLineage {
                digest: digest.to_owned(),
                cache: CacheOutcome::Hit,
                millis: None,
            }),
            services: Vec::new(),
            seed: Some(SeedLineage {
                digest: seed.map(str::to_owned),
                rebuilt: Some(false),
                millis: None,
            }),
            base: base.map(|key| BaseLineage {
                key: key.to_owned(),
                millis: None,
            }),
        }
    }

    fn summary(
        world: &str,
        state: WorldState,
        changed: bool,
        restarted: &[&str],
        lineage: WorldLineage,
    ) -> WorldUpSummary {
        let (project, fork) = match world.split_once(':') {
            Some((project, fork)) => (project, Some(fork)),
            None => (world, None),
        };
        WorldUpSummary {
            world: world.to_owned(),
            state,
            changed,
            restarted: restarted.iter().map(|id| (*id).to_owned()).collect(),
            worktree: PathBuf::from(format!("/home/dev/{world}")),
            origin: origin(project, fork, 80),
            lineage,
            services: Vec::new(),
            resources: Vec::new(),
            next: world_next_commands(state),
        }
    }

    fn canonical(changed: bool, restarted: &[&str]) -> WorldUpSummary {
        summary(
            "demo",
            WorldState::Running,
            changed,
            restarted,
            lineage(
                Some(&digest("a1")),
                Some(&digest("9f")),
                Some(&digest("45")),
            ),
        )
    }

    #[test]
    fn up_block_is_the_documented_order_with_one_compact_lineage_line() {
        let block = render(
            Verb::Up,
            &canonical(true, &["api"]),
            Options {
                qualify_next: false,
                verbose: false,
            },
        );
        let lines: Vec<&str> = block.lines().collect();
        assert_eq!(lines[0], "world     demo (running, changed)");
        assert_eq!(lines[1], "app       http://demo.localhost");
        assert_eq!(lines[2], "worktree  /home/dev/demo");
        assert_eq!(
            lines[3],
            "next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down"
        );
        assert_eq!(
            lines[4],
            "lineage   rootfs a10000000000 cached · seed 9f0000000000 cached · base 450000000000 reused"
        );
        assert_eq!(lines.len(), 5, "the happy path stays five rows: {block}");
        assert_eq!(
            block.matches("lineage").count(),
            1,
            "exactly one lineage line"
        );
        for jargon in ["[origin]", "[resources]", "[lineage]"] {
            assert!(!block.contains(jargon), "no happy-path jargon: {jargon}");
        }
    }

    #[test]
    fn restart_names_the_daemons_restarted_ids_or_the_honest_no_op() {
        let restarted = render(
            Verb::Restart,
            &canonical(true, &["api", "postgres"]),
            Options {
                qualify_next: false,
                verbose: false,
            },
        );
        let lines: Vec<&str> = restarted.lines().collect();
        assert_eq!(lines[0], "[restarted] api, postgres");
        assert_eq!(lines[1], "world     demo (running, changed)");
        assert_eq!(
            lines.len(),
            6,
            "the verdict leads the five-row block: {restarted}"
        );

        // `changed: false` is the daemon's observed fact: nothing restarted, nothing ran.
        let unchanged = render(
            Verb::Restart,
            &canonical(false, &[]),
            Options {
                qualify_next: false,
                verbose: false,
            },
        );
        let lines: Vec<&str> = unchanged.lines().collect();
        assert_eq!(lines[0], "[no change]");
        assert_eq!(lines[1], "world     demo (running, unchanged)");
        assert!(!unchanged.contains("[restarted]"));
        assert!(
            !unchanged.contains("apply edits"),
            "the up-only hint never blames restart: {unchanged}"
        );

        // A hook-only change has no restarted id to name, so it never invents one.
        let hook_only = render(
            Verb::Restart,
            &canonical(true, &[]),
            Options {
                qualify_next: false,
                verbose: false,
            },
        );
        assert!(hook_only.starts_with("[changed]\n"), "{hook_only}");
    }

    #[test]
    fn up_of_a_running_world_says_unchanged_and_names_restart() {
        let block = render(
            Verb::Up,
            &canonical(false, &[]),
            Options {
                qualify_next: false,
                verbose: false,
            },
        );
        assert!(
            block.starts_with(
                "world     demo (running, unchanged — run `slipfork restart` to apply edits)\n"
            ),
            "{block}"
        );
        assert!(
            !block.contains("[no change]"),
            "the verdict is restart-only"
        );
    }

    #[test]
    fn fork_adds_cd_and_qualifies_every_next_command() {
        let fork = summary(
            "demo:feat-ux",
            WorldState::Running,
            true,
            &["api"],
            lineage(Some(&digest("a1")), None, None),
        );
        let block = render(
            Verb::Fork,
            &fork,
            Options {
                qualify_next: true,
                verbose: false,
            },
        );
        let lines: Vec<&str> = block.lines().collect();
        assert_eq!(lines[0], "world     demo:feat-ux (running, changed)");
        assert_eq!(lines[1], "app       http://feat-ux.demo.localhost");
        assert_eq!(lines[2], "worktree  /home/dev/demo:feat-ux");
        assert_eq!(lines[3], "cd        cd /home/dev/demo:feat-ux");
        assert_eq!(
            lines[4],
            "next      slipfork open --world demo:feat-ux | slipfork db --world demo:feat-ux | \
             slipfork logs --world demo:feat-ux | slipfork restart --world demo:feat-ux | \
             slipfork down --world demo:feat-ux"
        );
        assert!(lines[5].starts_with("lineage   rootfs a10000000000 cached"));
        assert!(
            !block.contains("[restarted]"),
            "up/fork carry no verdict line"
        );
    }

    #[test]
    fn verbose_adds_resources_and_the_per_node_table_without_inventing_a_fact() {
        let mut full = summary(
            "demo",
            WorldState::Running,
            true,
            &["api"],
            WorldLineage {
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
                    millis: Some(2_104),
                }),
                base: Some(BaseLineage {
                    key: digest("4570132bffb8"),
                    millis: Some(10),
                }),
            },
        );
        full.resources = vec![ResourceSummary {
            id: "demo".to_owned(),
            strategy: "prefer_remote".to_owned(),
            resolved: "local".to_owned(),
            reason: "unpaired".to_owned(),
        }];
        let block = render(
            Verb::Up,
            &full,
            Options {
                qualify_next: false,
                verbose: true,
            },
        );
        assert!(
            block.contains(
                "lineage   rootfs a1b2c3d4e5f6 rebuilt 2.1s · postgres:18 cached · \
                 seed 9f8e7d6c5b4a rebuilt 2.1s · base 4570132bffb8 sealed 10ms\n"
            ),
            "the compact line stays the §8.2 renderer: {block}"
        );
        assert!(
            block.contains("[resources] demo: prefer_remote -> local (unpaired)\n"),
            "{block}"
        );
        for expected in [
            "[lineage]   rootfs       a1b2c3d4e5f6   rebuilt       2104ms\n",
            "[lineage]   postgres:18  -              cached        -\n",
            "[lineage]   seed         9f8e7d6c5b4a   rebuilt       2104ms\n",
            "[lineage]   base         4570132bffb8   sealed        10ms\n",
        ] {
            assert!(
                block.contains(expected),
                "missing `{expected}` in:\n{block}"
            );
        }

        // An all-unknown lineage is four dash rows, never `0` or a fabricated duration.
        let unknown = render(
            Verb::Up,
            &summary(
                "demo",
                WorldState::Running,
                true,
                &["api"],
                WorldLineage {
                    rootfs: None,
                    services: Vec::new(),
                    seed: None,
                    base: None,
                },
            ),
            Options {
                qualify_next: false,
                verbose: true,
            },
        );
        for node in ["rootfs", "seed", "base"] {
            assert!(
                unknown.contains(&format!(
                    "[lineage]   {node:<NODE_WIDTH$} -              -             -"
                )),
                "the unknown `{node}` fact is `-`: {unknown}"
            );
        }
        assert!(!unknown.contains(" 0"), "never a zero fact: {unknown}");
    }

    #[test]
    fn json_is_the_daemon_summary_plus_ok_and_nothing_else() {
        let mut summary = canonical(true, &["api"]);
        summary.services = vec![WorldServiceSummary {
            world: "demo".to_owned(),
            info: WorldServiceInfo {
                id: "api".to_owned(),
                kind: ServiceKind::Component,
                state: ServiceState::Running,
                pid: Some(4242),
                port: Some(8000),
                uptime_secs: Some(3),
            },
        }];
        let document = json(&summary).expect("the summary serializes");
        let keys: Vec<&str> = document
            .as_object()
            .expect("an object document")
            .keys()
            .map(String::as_str)
            .collect();
        for key in [
            "ok",
            "world",
            "state",
            "changed",
            "restarted",
            "worktree",
            "origin",
            "lineage",
            "services",
            "resources",
            "next",
        ] {
            assert!(
                keys.contains(&key),
                "the document carries `{key}`: {document}"
            );
        }
        assert_eq!(document["ok"], true);
        assert_eq!(document["changed"], true);
        assert_eq!(document["restarted"][0], "api");
        assert_eq!(document["origin"], "http://demo.localhost");
        assert_eq!(document["services"][0]["pid"], 4242);
        assert!(
            document["lineage"].get("rootfs").is_some(),
            "the daemon's lineage travels as-is: {document}"
        );
        let text = serde_json::to_string(&document).expect("json");
        for human in ["world     ", "lineage   ", "[restarted]"] {
            assert!(!text.contains(human), "no human text in --json: {human}");
        }
    }

    #[test]
    fn the_shared_lineage_renderer_keeps_the_verbose_table_vocabulary() {
        let lineage = WorldLineage {
            rootfs: Some(RootfsLineage {
                digest: digest("aa"),
                cache: CacheOutcome::Miss,
                millis: Some(1_500),
            }),
            services: vec![ServiceImageLineage {
                id: "redis:8".to_owned(),
                cache: CacheOutcome::Hit,
                millis: None,
            }],
            seed: Some(SeedLineage {
                digest: Some(digest("bb")),
                rebuilt: Some(false),
                millis: None,
            }),
            base: Some(BaseLineage {
                key: digest("cc"),
                millis: None,
            }),
        };
        let compact = render_lineage(&lineage);
        let table = detail(&lineage);
        for word in ["cached", "rebuilt", "reused"] {
            assert!(compact.contains(word), "compact: {compact}");
            assert!(table.contains(word), "detail: {table}");
        }
        for wire in ["hit", "miss"] {
            assert!(
                !compact.contains(wire) && !table.contains(wire),
                "no wire enum"
            );
        }
    }
}
