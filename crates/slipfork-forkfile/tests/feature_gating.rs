//! `features`/`on` truth table for the single [`FeatureSet`] gate.

use slipfork_forkfile::{Expr, FeatureSet, TopItem, parse};

const DECLARED: &str = r#"# forkfile: 0.1
features {
  dev
  prod
  default = [dev]
}
on dev: domain dev_only
on prod: domain prod_only
on staging: domain unknown_only
on !prod: domain not_prod
on dev & prod: domain both
on dev | prod: domain either
"#;

fn conditions(source: &str) -> (FeatureSet, Vec<Expr>) {
    let document = parse(source).expect("the feature fixture parses");
    let conditions = document
        .items
        .items
        .iter()
        .filter_map(|item| match item {
            TopItem::On(on) => Some(on.cond.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let features: FeatureSet = document.active_features();
    (features, conditions)
}

#[test]
fn on_conditions_follow_the_declared_default_features() {
    let (features, conds) = conditions(DECLARED);
    assert_eq!(conds.len(), 6, "every `on` declaration is collected");
    assert!(features.matches(&conds[0]), "`on dev` with `dev` active");
    assert!(
        !features.matches(&conds[1]),
        "`on prod` with `prod` inactive"
    );
    assert!(
        !features.matches(&conds[2]),
        "an undeclared feature stays inactive"
    );
    assert!(features.matches(&conds[3]), "`on !prod` negates");
    assert!(!features.matches(&conds[4]), "`and` needs both sides");
    assert!(features.matches(&conds[5]), "`or` accepts either side");
}

#[test]
fn an_empty_default_set_disables_every_bare_feature() {
    let (features, conds) =
        conditions("# forkfile: 0.1\nfeatures {\n  dev\n  default = []\n}\non dev: domain one\n");
    assert!(!features.matches(&conds[0]), "no default group activates");
}

#[test]
fn no_features_block_keeps_bare_conditions_enabled() {
    let (features, conds) = conditions("# forkfile: 0.1\non dev: domain one\n");
    assert!(features.matches(&conds[0]), "bare features pass undeclared");
}
