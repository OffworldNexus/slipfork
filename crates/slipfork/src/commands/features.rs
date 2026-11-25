//! `slipfork features`: list this world's feature configuration, or persist edits into
//! `<project>/.slipfork/features.json` for the next `up`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::process::ExitCode;

use serde_json::{Value, json};
use slipfork_forkfile::FeatureEdit;
use slipfork_forkfile::ast::Forkfile;

use crate::commands::{Session, emit};
use crate::ui::Palette;
use crate::ui::table::{Column, Table};
use crate::world::WorldIdentities;
use crate::world::features::WorldFeatures;

/// Mark, name, state, implications.
const COLUMNS: [Column; 4] = [
    Column::left(),
    Column::left(),
    Column::left(),
    Column::left(),
];

/// The header row, measured with the data so the grid never shifts.
const HEADER: [&str; 4] = ["", "FEATURE", "STATE", "IMPLIES"];

/// The one remedy for a `-F`/`-f`/`--flag` the shell let slip past clap.
const FLAG_NEXT: &str = "-F belongs to 'slipfork up/fork/env'; here pass +mail or -mail";

/// The one remedy for a malformed `CHANGES` batch.
const CHANGES_NEXT: &str = "run `slipfork features --help` to see the shipped forms";

/// One `apply_changes` refusal: the message plus its single `next:` remedy.
type ChangeError = (String, &'static str);

/// Parsed `slipfork features [CHANGES]... [-F FEATURES] [--no-default-features]` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeaturesArgs {
    /// `reset`, `enable <f>...`, `disable <f>...`, or ordered `+mail`/`-dev`/bare edits.
    pub changes: Vec<String>,
    /// `-F`/`--features` edits, merged before the positional `CHANGES`.
    pub features: Vec<FeatureEdit>,
    /// Start from an empty base instead of the built-in `dev` default.
    pub no_default_features: bool,
    /// Emit the machine-readable object with zero ANSI.
    pub json: bool,
}

impl Session {
    /// Prints every available feature, or validates and persists one edit batch.
    pub fn features(&self, args: &FeaturesArgs) -> ExitCode {
        let path = self.cwd().join("Forkfile");
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                return self.report(
                    "forkfile",
                    &format!("cannot read `{}`: {error}", path.display()),
                    "create a Forkfile in this directory, then run `slipfork up`",
                    args.json,
                );
            }
        };
        let checked = match slipfork_forkfile::compile(&source) {
            Ok(checked) => checked,
            Err(diagnostics) => {
                let first = diagnostics.first();
                let message = first.map_or_else(
                    || "invalid Forkfile".to_owned(),
                    |item| format!("Forkfile:{}: {}", item.span.line(), item.message),
                );
                let next = first.map_or("fix the Forkfile", |item| item.next.as_str());
                return self.report("forkfile", &message, next, args.json);
            }
        };
        let doc = &checked.document;
        let current = match WorldFeatures::load(self.cwd()) {
            Ok(current) => current,
            Err(error) => return self.report("feature", &error, WorldFeatures::REMEDY, args.json),
        };
        let (config, changed) = match apply_changes(current, args) {
            Ok(pair) => pair,
            Err((message, next)) => return self.report("usage", &message, next, args.json),
        };
        let resolved = match config.resolve(doc) {
            Ok(resolved) => resolved,
            Err(diagnostic) => {
                return self.report(
                    "feature",
                    &format!(
                        "Forkfile:{}: {}",
                        diagnostic.span.line(),
                        diagnostic.message
                    ),
                    &diagnostic.next,
                    args.json,
                );
            }
        };
        let identities = match WorldIdentities::open(self.host().state_dir.clone()) {
            Ok(identities) => identities,
            Err(error) => {
                return self.report(
                    "ledger",
                    &error,
                    "run `slipfork doctor` and retry",
                    args.json,
                );
            }
        };
        let world = match identities.name(self.cwd(), None) {
            Ok(world) => world.as_str().to_owned(),
            Err(error) => {
                return self.report(
                    "world",
                    &error,
                    "run `slipfork up` to register this checkout",
                    args.json,
                );
            }
        };
        if changed && let Err(error) = config.save(self.cwd()) {
            return self.report(
                "io",
                &format!(
                    "cannot save `{}`: {error}",
                    WorldFeatures::path(self.cwd()).display()
                ),
                "check that the project's `.slipfork` directory is writable",
                args.json,
            );
        }
        let implications = implications(doc, resolved.available());
        if args.json {
            let features: Vec<Value> = resolved
                .available()
                .iter()
                .map(|name| {
                    json!({
                        "name": name,
                        "active": resolved.contains(name),
                        "implies": implications.get(name).cloned().unwrap_or_default(),
                    })
                })
                .collect();
            return emit(&format!(
                "{}\n",
                json!({
                    "project": self.cwd().display().to_string(),
                    "world": world,
                    "no_default": config.no_default,
                    "edits": config.edits,
                    "active": resolved.active(),
                    "features": features,
                })
            ));
        }
        let palette = Palette::new(self.color());
        let table = Table::new(&COLUMNS);
        let mut plain: Vec<Vec<String>> = vec![HEADER.map(str::to_owned).to_vec()];
        let mut styled: Vec<Vec<String>> = vec![HEADER.map(|cell| palette.meta(cell)).to_vec()];
        for name in resolved.available() {
            let active = resolved.contains(name);
            let implies = implies_cell(&implications, name);
            plain.push(vec![
                if active { "●" } else { "○" }.to_owned(),
                name.clone(),
                if active { "active" } else { "inactive" }.to_owned(),
                implies.clone(),
            ]);
            styled.push(vec![
                palette.tint(
                    if active { "●" } else { "○" },
                    if active { "32" } else { "2" },
                ),
                if active {
                    palette.label(name)
                } else {
                    palette.meta(name)
                },
                if active {
                    palette.tint("active", "32")
                } else {
                    palette.meta("inactive")
                },
                palette.meta(&implies),
            ]);
        }
        let widths = table.widths(&plain);
        let mut text = String::new();
        for row in &styled {
            text.push_str(&table.line(&widths, row));
            text.push('\n');
        }
        if !config.is_default() {
            text.push_str(&format!("{}\n", palette.meta(&summary(&config))));
        }
        if changed {
            text.push_str(&format!(
                "{} {}\n",
                palette.kind("next:"),
                palette.command("run `slipfork up` to apply these features")
            ));
        }
        emit(&text)
    }
}

/// The persisted edits as one dim summary line (`edits: --no-default-features +mail -dev`).
fn summary(config: &WorldFeatures) -> String {
    let mut text = String::from("edits:");
    if config.no_default {
        text.push_str(" --no-default-features");
    }
    for edit in &config.edits {
        text.push(' ');
        text.push_str(&edit.to_string());
    }
    text
}

/// Every feature name expanded through the compiler's own implication walk.
fn implications(doc: &Forkfile, available: &BTreeSet<String>) -> BTreeMap<String, Vec<String>> {
    available
        .iter()
        .map(|name| {
            let implied = doc
                .resolve_features(true, &[FeatureEdit::Enable(name.clone())])
                .map_or_else(
                    |_| Vec::new(),
                    |set| {
                        set.active()
                            .iter()
                            .filter(|other| other.as_str() != name.as_str())
                            .cloned()
                            .collect()
                    },
                );
            (name.clone(), implied)
        })
        .collect()
}

/// One implies cell: compact `-> mail`, empty when the feature implies nothing.
fn implies_cell(implications: &BTreeMap<String, Vec<String>>, name: &str) -> String {
    let implied = implications.get(name).cloned().unwrap_or_default();
    if implied.is_empty() {
        String::new()
    } else {
        format!("-> {}", implied.join(", "))
    }
}

/// Applies one `-F`/`CHANGES` batch on top of the persisted configuration.
fn apply_changes(
    current: WorldFeatures,
    args: &FeaturesArgs,
) -> Result<(WorldFeatures, bool), ChangeError> {
    let mut config = if args.no_default_features {
        WorldFeatures::new(true, Vec::new())
    } else {
        current.clone()
    };
    for edit in &args.features {
        config.apply(std::slice::from_ref(edit));
    }
    match args.changes.first().map(String::as_str) {
        None => {}
        Some("reset") => {
            config = WorldFeatures::default();
            for change in &args.changes[1..] {
                config.apply(&[edit_of(change)?]);
            }
        }
        Some(verb @ ("enable" | "disable")) => {
            if args.changes.len() < 2 {
                return Err((
                    format!("`{verb}` needs at least one feature name"),
                    CHANGES_NEXT,
                ));
            }
            let prefix = if verb == "enable" { '+' } else { '-' };
            for name in &args.changes[1..] {
                config.apply(&[edit_of(&format!("{prefix}{name}"))?]);
            }
        }
        Some(_) => {
            for change in &args.changes {
                config.apply(&[edit_of(change)?]);
            }
        }
    }
    config.compact();
    let changed = config != current;
    Ok((config, changed))
}

/// Parses one positional edit, rejecting a flag the shell let slip past clap.
fn edit_of(change: &str) -> Result<FeatureEdit, ChangeError> {
    if change.starts_with("--") || matches!(change, "-F" | "-f") {
        return Err((format!("`{change}` is not a feature edit"), FLAG_NEXT));
    }
    change
        .parse::<FeatureEdit>()
        .map_err(|error| (error, CHANGES_NEXT))
}
