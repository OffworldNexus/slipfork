//! `fork` and `drop`: snapshot a live world into its own CoW checkout, or delete one.
//!
//! The daemon owns the Btrfs snapshot, the branch switch, and the fork's boot; this module only
//! validates the request's features against the parent's `Forkfile`, streams the typed events
//! into [`BootTui`], and prints exactly one `next:` remedy (or one `specs/02` JSON object).

use std::process::ExitCode;

use serde_json::json;

use crate::boot::{BootTui, dropped_row};
use crate::commands::Session;
use crate::ui::report::Failure;

use super::features::WorldFeatures;
use super::up::{emit_json, emit_stream};
use super::{DropArgs, ForkArgs};

impl Session {
    /// Forks the running world into its own CoW snapshots and boots it behind its own URL.
    pub fn fork(&self, args: &ForkArgs) -> ExitCode {
        let typed = !args.features.is_empty() || args.no_default_features;
        let request = WorldFeatures::new(args.no_default_features, args.features.clone());
        // Prove the fork's inherited configuration plus this request resolves before snapshotting.
        if typed && let Err(code) = self.project_features(self.cwd(), &request, args.json) {
            return code;
        }
        let params = if typed {
            json!({
                "project": self.cwd(),
                "name": args.name,
                "features": args.features,
                "no_default_features": args.no_default_features,
            })
        } else {
            json!({ "project": self.cwd(), "name": args.name })
        };
        let mut client = match self.connect() {
            Ok(client) => client,
            Err(error) => {
                return self.lifecycle_failure(
                    Failure::remedied(
                        error.code.clone(),
                        error.message.clone(),
                        "run `slipfork up` first",
                    ),
                    args.json,
                    false,
                );
            }
        };
        let mut tui = BootTui::boot(
            self.ui(),
            self.terminal().cols,
            &format!("Forking {}", args.name.as_deref().unwrap_or("fork")),
            args.verbose,
        );
        let outcome = client.call_streaming("fork", params, &mut |event| {
            emit_stream(args.json, args.verbose, event);
            tui.observe(event);
        });
        match outcome {
            Ok(reply) => {
                if args.json {
                    tui.finish();
                    return emit_json(&json!({
                        "fork": reply["fork"],
                        "world": reply["world"],
                        "project": reply["project"],
                        "url": reply["url"],
                        "elapsed_ms": reply["elapsed_ms"],
                        "snapshot_ms": reply["snapshot_ms"],
                    }));
                }
                tui.finish();
                ExitCode::SUCCESS
            }
            Err(error) => {
                tui.finish();
                let next = match error.code.as_str() {
                    "not-btrfs" => {
                        "move the project onto a btrfs mount, then retry `slipfork fork`".to_owned()
                    }
                    "nested-mount" => {
                        "unmount the nested mount, then retry `slipfork fork`".to_owned()
                    }
                    "unsupported-entry" => {
                        "remove the FIFO, socket, or device node from the checkout, then retry `slipfork fork`".to_owned()
                    }
                    "not-subvolume" => {
                        "remove or move aside the entry that is not a Slipfork subvolume, then retry `slipfork fork`".to_owned()
                    }
                    "git-branch" | "git-missing" | "invalid-fork" => {
                        "pass a fork name: `slipfork fork <name>`".to_owned()
                    }
                    "no-world" => "run `slipfork up` first".to_owned(),
                    "world-booting" => {
                        "wait for `slipfork up` to finish, then retry `slipfork fork`".to_owned()
                    }
                    "pg-checkpoint" => {
                        "run `slipfork logs pg` if PostgreSQL stays unhealthy, then retry `slipfork fork`".to_owned()
                    }
                    "world-running" => {
                        "pick another name, or drop the existing fork first".to_owned()
                    }
                    "forkfile" => "restore the file, then retry `slipfork fork`".to_owned(),
                    "boot-failed" => {
                        "run `slipfork logs` for the failing step, then retry `slipfork up`"
                            .to_owned()
                    }
                    _ => "run `slipfork status` to inspect the world".to_owned(),
                };
                self.lifecycle_failure(
                    Failure::remedied(error.code.clone(), error.message.clone(), next),
                    args.json,
                    tui.failed(),
                )
            }
        }
    }

    /// Stops a fork and deletes both of its snapshots.
    pub fn drop(&self, args: &DropArgs) -> ExitCode {
        let mut client = match self.connect() {
            Ok(client) => client,
            Err(error) => {
                return self.lifecycle_failure(
                    Failure::remedied(
                        error.code.clone(),
                        error.message.clone(),
                        "run `slipfork status` to inspect the world",
                    ),
                    args.json,
                    false,
                );
            }
        };
        let mut tui = BootTui::rows(
            self.ui(),
            self.terminal().cols,
            &format!("Dropping {}", args.name),
        );
        let outcome = client.call_streaming(
            "drop",
            json!({ "project": self.cwd(), "name": args.name, "force": args.force }),
            &mut |event| {
                emit_stream(args.json, false, event);
                tui.observe(event);
            },
        );
        match outcome {
            Ok(reply) => {
                let elapsed = reply["elapsed_ms"].as_u64().unwrap_or(0);
                if args.json {
                    tui.finish();
                    return emit_json(
                        &json!({ "dropped": reply["dropped"], "elapsed_ms": elapsed }),
                    );
                }
                tui.line(&dropped_row(&args.name, elapsed, self.color()));
                tui.finish();
                ExitCode::SUCCESS
            }
            Err(error) => {
                tui.finish();
                let next = match error.code.as_str() {
                    "dirty-fork" => format!("slipfork drop {} -f", args.name),
                    "not-btrfs" => {
                        "move the project onto a btrfs mount, then retry `slipfork drop`".to_owned()
                    }
                    "no-fork" => "run `slipfork ps` to list this world's forks".to_owned(),
                    _ => "run `slipfork status` to inspect the world".to_owned(),
                };
                self.lifecycle_failure(
                    Failure::remedied(error.code.clone(), error.message.clone(), next),
                    args.json,
                    tui.failed(),
                )
            }
        }
    }
}
