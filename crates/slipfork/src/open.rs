//! `slipfork open` and `slipfork open --remote` (`docs/design/dx-0.1.0-ux.md` §6).
//!
//! Every function here is pure over the daemon's own facts: the world table
//! ([`WorldSummary`] rows, each with one typed [`WorldOrigin`]), the ingress port from
//! [`DaemonStatus`](slipfork_wire::control::DaemonStatus), and the composition root's resolved
//! [`UserIdentity`](crate::user::UserIdentity) value and the `uname -n` host name. The local verb
//! hands the typed origin to the one concrete [`Browser`](crate::browser::Browser); the remote verb
//! never opens anything and never invents a port — the laptop forwards the fixed constant
//! [`LOCAL_PORT`] to the daemon's real `127.0.0.1:<ingress.port>` listener.
//!
//! **Why `http://<route>.localhost:8080` routes (verified, `M3.28.1`):** browsers resolve
//! `*.localhost` to loopback and send `Host: <route>.localhost:8080` through the forwarded port;
//! `LocalHostRoute::parse` strips a numeric `:port`, so the forwarded `Host` resolves to exactly
//! the world whose origin was printed. A unit test round-trips the forwarded `Host` header, so a
//! recipe the edge would reject can never ship.

use serde::Serialize;
use slipfork_vcs::{RepoRoot, WORKTREES_DIRECTORY};
use slipfork_wire::control::WorldSummary;
use slipfork_world::WorldOrigin;

/// The fixed laptop-side forward port of the `open --remote` recipe (there is no `--local-port`).
pub const LOCAL_PORT: u16 = 8080;

/// One world the CLI selected: its canonical id and the daemon's typed origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldSelection {
    /// Canonical world id (`<project>` or `<project>:<fork>`) exactly as `slipfork list` prints it.
    pub world: String,
    /// The world's single origin, minted and validated by the daemon.
    pub origin: WorldOrigin,
}

/// The verified laptop recipe: one `ssh` command and the URL to browse through the tunnel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteRecipe {
    /// `ssh -N -L 8080:127.0.0.1:<ingress.port> <account>@<host>`.
    pub ssh: String,
    /// `http://<route>.localhost:8080` (the origin host, never its daemon-side port).
    pub url: String,
}

/// Resolve the world `--world` / the calling checkout selects from the daemon's world table.
///
/// `selector` is matched first as the exact canonical id every command prints
/// (`<project>` / `<project>:<fork>`); otherwise it must be a bare `<fork>` label naming exactly
/// one world. Without `--world`, the world whose daemon-reported worktree is this checkout wins
/// (`repo.worktree_root` is the canonical checkout or `.worktrees/<fork>`).
///
/// An unknown world (or an ambiguous bare fork) fails closed in the one `unknown_world` line: a
/// read verb never guesses which world the developer meant.
pub fn resolve(
    worlds: &[WorldSummary],
    repo: Option<&RepoRoot>,
    selector: Option<&str>,
) -> Result<WorldSelection, String> {
    match selector {
        Some(raw) => {
            let raw = raw.trim();
            if raw.is_empty() {
                return Err(
                    "a world selector must not be empty; pass `<fork>`, `<project>:<fork>`, or \
                     `<project>`"
                        .to_owned(),
                );
            }
            if let Some(world) = worlds.iter().find(|world| world.world == raw) {
                return Ok(selection(world));
            }
            let mut candidates: Vec<&WorldSummary> = worlds
                .iter()
                .filter(|world| {
                    world
                        .world
                        .rsplit_once(':')
                        .is_some_and(|(_, fork)| fork == raw)
                })
                .collect();
            match candidates.len() {
                1 => Ok(selection(
                    candidates.pop().expect("one candidate was counted"),
                )),
                0 => Err(unknown_world(raw)),
                _ => Err(format!(
                    "world '{raw}' is ambiguous; pass the project-qualified id ({})",
                    candidates
                        .iter()
                        .map(|world| world.world.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }
        None => {
            let repo = repo.ok_or_else(|| {
                "`slipfork open` needs a checkout to select a world; pass `--world <WORLD>` from \
                 outside a repository"
                    .to_owned()
            })?;
            let expected = checkout_world_id(repo);
            worlds
                .iter()
                .find(|world| world.worktree == repo.worktree_root)
                .map(selection)
                .ok_or_else(|| unknown_world(&expected))
        }
    }
}

/// The checked-out world's canonical id (`<project>` or `<project>:<fork>`).
fn checkout_world_id(repo: &RepoRoot) -> String {
    let worktrees = repo.vcs_root.join(WORKTREES_DIRECTORY);
    match repo.worktree_root.strip_prefix(&worktrees) {
        Ok(relative) => match relative
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
        {
            Some(fork) => format!("{}:{fork}", repo.project.as_str()),
            None => repo.project.as_str().to_owned(),
        },
        Err(_) => repo.project.as_str().to_owned(),
    }
}

fn selection(world: &WorldSummary) -> WorldSelection {
    WorldSelection {
        world: world.world.clone(),
        origin: world.origin.clone(),
    }
}

/// The one `unknown_world` diagnostic (`docs/design/dx-0.1.0-ux.md` §5, §10).
fn unknown_world(world: &str) -> String {
    format!(
        "no world '{world}' here; boot it with `slipfork up` or list worlds with `slipfork list`"
    )
}

/// The verified recipe from the daemon's real ingress port and the composition root's account.
///
/// The local port is always [`LOCAL_PORT`] and the tunnel target is the daemon's own
/// `127.0.0.1:<ingress_port>`; a reported `:0` fails closed instead of printing a recipe that
/// cannot connect.
pub fn remote_recipe(
    origin: &WorldOrigin,
    ingress_port: u16,
    account: &str,
    host: &str,
) -> Result<RemoteRecipe, String> {
    // `kernel_hostname()` yields the empty string when `uname -n` is unusable; a recipe without a
    // real host would hand the developer a command that cannot connect.
    if host.is_empty() {
        return Err(
            "cannot resolve this machine's name: `uname -n` returned no usable node name; the \
             `open --remote` recipe needs a host usable as `account@host`"
                .to_owned(),
        );
    }
    if ingress_port == 0 {
        return Err(
            "the daemon reported ingress :0; a remote recipe needs the real bound port — restart \
             slipforkd so it reports one"
                .to_owned(),
        );
    }
    Ok(RemoteRecipe {
        ssh: format!("ssh -N -L {LOCAL_PORT}:127.0.0.1:{ingress_port} {account}@{host}"),
        url: format!("http://{}:{LOCAL_PORT}", origin.host()),
    })
}

/// The human `open --remote` block: the world, the line to paste on the laptop, and its URL.
pub fn render_remote(selection: &WorldSelection, recipe: &RemoteRecipe) -> String {
    format!(
        "{:<8}{}\n{:<8}{}\n{:<8}{}\n",
        "world", selection.world, "ssh", recipe.ssh, "app", recipe.url
    )
}

/// The human local confirmation (`opened    http://<route>.localhost`).
pub fn render_local(selection: &WorldSelection) -> String {
    format!("opened    {}\n", selection.origin.url())
}

/// The `open --json` document (`docs/design/dx-0.1.0-ux.md` §10): every key is always present,
/// and `recipe` is `null` for a local open.
pub fn json(selection: &WorldSelection, recipe: Option<&RemoteRecipe>) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "world": selection.world,
        "origin": selection.origin.url(),
        "recipe": recipe,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use slipfork_wire::control::WorldState;
    use slipfork_world::WorldLineage;
    use slipfork_world::{LocalHostRoute, ProjectSlug, WorldTarget};

    use super::*;

    fn slug(name: &str) -> ProjectSlug {
        ProjectSlug::parse(name).expect("a valid project slug")
    }

    fn origin(project_name: &str, fork: Option<&str>, ingress_port: u16) -> WorldOrigin {
        let target = fork.map_or(WorldTarget::Canonical, |fork| {
            WorldTarget::UserFork(fork.to_owned())
        });
        let route = LocalHostRoute::new(slug(project_name), target, None, &[])
            .expect("a routable .localhost route");
        WorldOrigin::local(&route, ingress_port).expect("a minted origin")
    }

    fn world(id: &str, worktree: &str, ingress_port: u16) -> WorldSummary {
        let (project, fork) = match id.split_once(':') {
            Some((project, fork)) => (project, Some(fork)),
            None => (id, None),
        };
        WorldSummary {
            world: id.to_owned(),
            parent: fork.map(|_| project.to_owned()),
            state: WorldState::Running,
            origin: origin(project, fork, ingress_port),
            worktree: PathBuf::from(worktree),
            lineage: WorldLineage {
                rootfs: None,
                services: Vec::new(),
                seed: None,
                base: None,
            },
        }
    }

    fn repo(project_name: &str, fork: Option<&str>) -> RepoRoot {
        let vcs_root = PathBuf::from(format!("/home/dev/{project_name}"));
        let worktree_root = match fork {
            Some(fork) => vcs_root.join(WORKTREES_DIRECTORY).join(fork),
            None => vcs_root.clone(),
        };
        RepoRoot {
            vcs_root,
            worktree_root,
            project: slug(project_name),
        }
    }

    #[test]
    fn open_resolves_the_calling_checkout_and_exact_and_bare_ids() {
        let worlds = vec![
            world("demo", "/home/dev/demo", 80),
            world("demo:feat-ux", "/home/dev/demo/.worktrees/feat-ux", 80),
            world("other", "/home/dev/other", 80),
        ];

        let canonical = resolve(&worlds, Some(&repo("demo", None)), None).expect("cwd world");
        assert_eq!(canonical.world, "demo");
        assert_eq!(canonical.origin.url(), "http://demo.localhost");

        let fork = resolve(&worlds, Some(&repo("demo", Some("feat-ux"))), None)
            .expect("a fork checkout selects its own fork");
        assert_eq!(fork.world, "demo:feat-ux");
        assert_eq!(fork.origin.url(), "http://feat-ux.demo.localhost");

        // The exact canonical id works from anywhere, including another project's checkout.
        let exact = resolve(&worlds, Some(&repo("other", None)), Some("demo:feat-ux"))
            .expect("an exact id is a fleet-wide lookup");
        assert_eq!(exact.world, "demo:feat-ux");

        let bare = resolve(&worlds, None, Some("feat-ux")).expect("a unique bare fork label");
        assert_eq!(bare.world, "demo:feat-ux");

        let unknown = resolve(&worlds, Some(&repo("demo", None)), Some("nope"))
            .expect_err("an unknown world fails closed");
        assert_eq!(
            unknown,
            "no world 'nope' here; boot it with `slipfork up` or list worlds with `slipfork list`"
        );
        assert!(resolve(&worlds, None, None).is_err());
        assert!(resolve(&worlds, None, Some("  ")).is_err());
    }

    #[test]
    fn a_bare_fork_that_names_two_worlds_fails_closed_with_both_ids() {
        let worlds = vec![
            world("demo", "/home/dev/demo", 80),
            world("demo:feat", "/home/dev/demo/.worktrees/feat", 80),
            world("other:feat", "/home/dev/other/.worktrees/feat", 80),
        ];
        let error = resolve(&worlds, None, Some("feat")).expect_err("ambiguous");
        assert!(
            error.contains("demo:feat") && error.contains("other:feat"),
            "{error}"
        );
        assert!(error.contains("ambiguous"));
    }

    #[test]
    fn the_remote_recipe_uses_the_real_ingress_port_and_the_constant_8080() {
        let origin = origin("demo", None, 40645);
        let recipe = remote_recipe(&origin, 40645, "slipfork", "slipfork-dev-vm")
            .expect("a bound ingress port");
        assert_eq!(
            recipe.ssh,
            "ssh -N -L 8080:127.0.0.1:40645 slipfork@slipfork-dev-vm"
        );
        assert_eq!(recipe.url, "http://demo.localhost:8080");
        assert!(recipe.ssh.contains("8080:127.0.0.1:40645"));
        assert!(
            remote_recipe(&origin, 0, "slipfork", "slipfork-dev-vm").is_err(),
            "a :0 recipe can never connect"
        );
    }

    #[test]
    fn the_remote_recipe_host_header_round_trips_through_local_host_route() {
        for (project_name, fork) in [("demo", None), ("demo", Some("feat-ux"))] {
            let route = LocalHostRoute::new(
                slug(project_name),
                fork.map_or(WorldTarget::Canonical, |fork| {
                    WorldTarget::UserFork(fork.to_owned())
                }),
                None,
                &[],
            )
            .expect("a routable route");
            let origin = WorldOrigin::local(&route, 40645).expect("a minted origin");
            let recipe = remote_recipe(&origin, 40645, "dev", "vm").expect("a bound ingress port");

            // The browser forwards the URL's authority; `LocalHostRoute::parse` strips `:8080`.
            let authority = recipe
                .url
                .strip_prefix("http://")
                .expect("the recipe URL is http");
            let parsed = LocalHostRoute::parse(&slug(project_name), &[], authority)
                .unwrap_or_else(|| panic!("the edge rejects its own recipe: {authority}"));
            assert_eq!(
                parsed, route,
                "`Host: {authority}` must resolve to the world whose origin was printed"
            );
            assert_eq!(parsed.host(), route.host());
        }
    }

    #[test]
    fn human_blocks_and_json_are_the_documented_shapes() {
        let summary = world("demo", "/home/dev/demo", 40645);
        let selected = selection(&summary);
        assert_eq!(
            render_local(&selected),
            "opened    http://demo.localhost:40645\n"
        );

        let recipe = remote_recipe(&selected.origin, 40645, "slipfork", "slipfork-dev-vm")
            .expect("a bound ingress port");
        assert_eq!(
            render_remote(&selected, &recipe),
            "world   demo\n\
             ssh     ssh -N -L 8080:127.0.0.1:40645 slipfork@slipfork-dev-vm\n\
             app     http://demo.localhost:8080\n"
        );

        let document = json(&selected, Some(&recipe));
        assert_eq!(document["ok"], true);
        assert_eq!(document["world"], "demo");
        assert_eq!(document["origin"], "http://demo.localhost:40645");
        assert_eq!(document["recipe"]["ssh"], recipe.ssh);
        assert_eq!(document["recipe"]["url"], recipe.url);

        let local = json(&selected, None);
        assert!(local["recipe"].is_null(), "a local open has no recipe");
        let text = serde_json::to_string(&local).expect("json");
        assert!(!text.contains("opened"), "no human text: {text}");
    }
}
