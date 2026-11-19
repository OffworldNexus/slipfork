//! The `-F/--features` + `--no-default-features` arguments.
//!
//! The selection is evaluated by `slipforkd` against the manifest's own feature table, so the CLI
//! never re-parses `Forkfile` syntax.

use clap::Args;
use slipfork_wire::control::FeatureSelection;

/// `-F/--features <list>` plus `--no-default-features` for one invocation.
#[derive(Debug, Default, Args)]
pub struct FeatureArgs {
    /// Extra `Forkfile` features to enable (`-F api,worker`).
    #[arg(short = 'F', long, value_name = "FEATURES", value_delimiter = ',')]
    pub features: Vec<String>,
    /// Disable the manifest's default feature set for this invocation.
    #[arg(long)]
    pub no_default_features: bool,
}

impl FeatureArgs {
    /// The wire selection this invocation carries.
    #[must_use]
    pub fn selection(&self) -> FeatureSelection {
        FeatureSelection {
            features: self.features.clone(),
            no_default_features: self.no_default_features,
            ..FeatureSelection::default()
        }
    }
}
