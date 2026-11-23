//! `features`/`on` truth table for the single [`FeatureSet`] gate.

use slipfork_forkfile::{Expr, FeatureEdit, FeatureSet, Forkfile, TopItem, parse};

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
    let features: FeatureSet = document.default_features();
    (features, conditions)
}

fn edit(text: &str) -> FeatureEdit {
    text.parse().expect("a valid feature edit")
}

#[test]
fn on_conditions_follow_the_default_features() {
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
fn dev_is_built_in_even_without_a_features_block() {
    let (features, conds) =
        conditions("# forkfile: 0.1\non dev: domain one\non mail: domain two\n");
    assert!(features.contains("dev"), "`dev` is always available");
    assert!(features.matches(&conds[0]), "`on dev` is on by default");
    assert!(
        !features.matches(&conds[1]),
        "a bare `on mail` is false until `mail` is active"
    );
}

#[test]
fn a_declared_dev_group_expands_its_implications() {
    let document =
        parse("# forkfile: 0.1\nfeatures {\n  dev = [mail]\n  mail\n}\non mail: domain inbox\n")
            .expect("parses");
    let features = document
        .resolve_features(false, &[])
        .expect("`dev` resolves its implication");
    assert!(features.contains("dev"));
    assert!(features.contains("mail"), "`dev = [mail]` enables `mail`");
    assert!(document.available_features().contains("mail"));
}

#[test]
fn no_default_features_starts_empty_and_edits_apply_in_order() {
    let document = parse(
        "# forkfile: 0.1\nfeatures {\n  dev\n  mail\n  default = [dev, mail]\n}\non dev: domain dev_only\non mail: domain inbox\n",
    )
    .expect("parses");

    let defaulted = document
        .resolve_features(false, &[])
        .expect("the default set resolves");
    assert!(defaulted.contains("dev"), "`dev` is built in");
    let empty = document
        .resolve_features(true, &[])
        .expect("the empty base resolves");
    assert!(
        empty.active().is_empty(),
        "no-default-features starts empty"
    );

    let mail = document
        .resolve_features(true, &[edit("+mail")])
        .expect("+mail resolves");
    assert!(mail.contains("mail") && !mail.contains("dev"));

    let bare = document
        .resolve_features(true, &[edit("mail")])
        .expect("a bare name enables");
    assert!(bare.contains("mail"));

    let no_dev = document
        .resolve_features(false, &[edit("-dev")])
        .expect("-dev resolves");
    assert!(!no_dev.contains("dev"));

    let no_mail = document
        .resolve_features(false, &[edit("!mail")])
        .expect("!mail resolves");
    assert!(!no_mail.contains("mail"));
    assert!(
        no_mail.contains("dev"),
        "disabling `mail` keeps the built-in `dev` active"
    );
}

#[test]
fn an_unknown_feature_names_the_available_set() {
    let document = parse("# forkfile: 0.1\nfeatures {\n  mail\n}\n").expect("parses");
    let error = document
        .resolve_features(false, &[FeatureEdit::Enable("ghost".into())])
        .expect_err("`ghost` is unknown");
    assert_eq!(error.message, "unknown feature `ghost`");
    assert_eq!(
        error.next,
        "use one of dev, mail or declare it in `features { }`"
    );
}

#[test]
fn services_with_filters_on_gated_declarations() {
    let source = "# forkfile: 0.1\nfeatures {\n  mail\n}\nimage app . {\n  service web { port 8000 }\n  on mail { service mailpit { port 8025 } }\n}\n";
    let document = parse(source).expect("parses");
    let ids = |features: &FeatureSet| {
        document
            .services_with(features)
            .iter()
            .map(|service| service.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(&document.default_features()),
        ["web"],
        "a mail-gated service stays out while mail is off"
    );
    let mail = document
        .resolve_features(false, &[edit("+mail")])
        .expect("+mail resolves");
    assert_eq!(ids(&mail), ["web", "mailpit"]);
}

#[test]
fn disabling_a_group_suppresses_the_features_only_it_implies() {
    fn service_ids(document: &Forkfile, features: &FeatureSet) -> Vec<String> {
        document
            .services_with(features)
            .iter()
            .map(|service| service.id.to_owned())
            .collect()
    }

    let document = parse(
        "# forkfile: 0.1\nfeatures {\n  dev = [mail]\n  mail\n}\nimage app . {\n  service web { port 8000 }\n  on mail { service mailpit { port 8025 } }\n}\n",
    )
    .expect("parses");
    let resolve = |no_default: bool, texts: &[&str]| {
        let edits = texts.iter().map(|text| edit(text)).collect::<Vec<_>>();
        document
            .resolve_features(no_default, &edits)
            .expect("edits resolve")
    };
    let names = |features: &FeatureSet| features.active().iter().cloned().collect::<Vec<_>>();

    let defaulted = resolve(false, &[]);
    assert_eq!(
        names(&defaulted),
        ["dev", "mail"],
        "`dev = [mail]` implies `mail` by default"
    );
    assert_eq!(service_ids(&document, &defaulted), ["web", "mailpit"]);

    let prod = resolve(false, &["-dev"]);
    assert!(
        names(&prod).is_empty(),
        "`-dev` also retires the `mail` only it implies"
    );
    assert_eq!(
        service_ids(&document, &prod),
        ["web"],
        "mailpit stays off in the prod shape"
    );

    assert_eq!(names(&resolve(false, &["!mail"])), ["dev"]);
    assert_eq!(
        names(&resolve(false, &["+mail", "-dev"])),
        ["mail"],
        "an explicit enable outlives the group that implied it"
    );
    assert!(
        names(&resolve(true, &[])).is_empty(),
        "--no-default-features starts empty"
    );
}

#[test]
fn explicit_default_membership_survives_disabling_dev() {
    let document = parse("# forkfile: 0.1\nfeatures {\n  default = [mail]\n  dev\n  mail\n}\n")
        .expect("parses");
    let resolve = |texts: &[&str]| {
        let edits = texts.iter().map(|text| edit(text)).collect::<Vec<_>>();
        document
            .resolve_features(false, &edits)
            .expect("edits resolve")
    };
    let names = |features: &FeatureSet| features.active().iter().cloned().collect::<Vec<_>>();

    assert_eq!(names(&resolve(&[])), ["dev", "mail"]);
    assert_eq!(
        names(&resolve(&["-dev"])),
        ["mail"],
        "`default = [mail]` is a root of its own, so `mail` survives `-dev`"
    );
}

#[test]
fn feature_edits_display_and_parse_round_trip() {
    assert_eq!(edit("!mail").to_string(), "-mail");
    assert_eq!(edit("+mail").to_string(), "+mail");
    for text in ["mail", "+mail", "-mail", "!mail", "+ci-test"] {
        let parsed = edit(text);
        assert_eq!(
            FeatureEdit::try_from(parsed.to_string()),
            Ok(parsed.clone())
        );
        assert_eq!(FeatureEdit::try_from(format!(" {parsed} ")), Ok(parsed));
    }
    assert!(
        "--mail".parse::<FeatureEdit>().is_err(),
        "a double prefix is an invalid edit, not a feature named `-mail`"
    );
}
