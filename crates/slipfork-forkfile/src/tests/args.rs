use super::*;

const BOUND: &str = r#"# forkfile: 0.1

postgres pg { database app_db }
redis cache

image api . {
  arg DB_URL = app_db.url
  arg REDIS_PORT = cache.port
  arg PORT = web.port
  arg NAME = web.name
  on dev { arg PORT = "5173" }

  service web {
    port 3000
    route /
    run: app --port $port
  }
}
"#;

#[test]
fn service_properties_bind_into_build_args() {
    let doc = parse(BOUND).expect("parses");
    let checked = check(&doc).expect("checks");
    assert_eq!(
        checked.symbols.get("web").and_then(|symbol| symbol.port),
        Some(3000)
    );
    let dev = doc.default_features();
    let prod = doc.resolve_features(true, &[]).expect("valid features");
    let prod_args = doc.eval_image_args_with("acme", "http", "api", &prod);
    let dev_args = doc.eval_image_args_with("acme", "http", "api", &dev);
    assert_eq!(
        prod_args["PORT"], "3000",
        "without `dev` the base `arg` wins"
    );
    assert_eq!(
        dev_args["PORT"], "5173",
        "an active `on dev` arg overrides the base value"
    );
    assert_eq!(prod_args["NAME"], "web");
    assert_eq!(
        prod_args["DB_URL"],
        "postgresql://postgres@127.0.0.1:5432/app_db"
    );
    assert_eq!(prod_args["REDIS_PORT"], "6379");
}

const NOPE: &str = r#"# forkfile: 0.1

image api . {
  arg X = web.nope

  service web {
    port 3000
    route /
    run: app
  }
}
"#;

#[test]
fn an_unknown_service_property_names_the_arg_and_the_valid_properties() {
    let doc = parse(NOPE).expect("parses");
    let (_, diags) = check(&doc).expect_err("`nope` is not a service property");
    let rendered = diags.render(NOPE);
    assert!(rendered.contains("Forkfile:4:"), "{rendered}");
    assert!(
        rendered.contains("`arg X`: `web` has no property `nope`"),
        "{rendered}"
    );
    assert!(
        rendered.contains("next: use `web.` followed by one of `port`, `name`"),
        "{rendered}"
    );
}

const GHOST: &str = "# forkfile: 0.1\n\nimage api . {\n  arg X = ghost.port\n\n  service web {\n    port 3000\n    route /\n    run: app\n  }\n}\n";

#[test]
fn an_unknown_base_names_the_arg_and_the_symbol() {
    let doc = parse(GHOST).expect("parses");
    let (_, diags) = check(&doc).expect_err("`ghost` is undeclared");
    let rendered = diags.render(GHOST);
    assert!(
        rendered.contains("`arg X`: unknown symbol `ghost`"),
        "{rendered}"
    );
    assert!(
        rendered.contains("next: declare `ghost` before referencing `ghost.port`"),
        "{rendered}"
    );
}

const SCOPED_PORT: &str = r#"# forkfile: 0.1

image api . {
  arg X = worker.port

  service worker {
    run: uv run worker
    on dev { port 9000 }
  }
}
"#;

#[test]
fn a_port_scoped_to_an_on_branch_is_refused_not_silently_dropped() {
    let doc = parse(SCOPED_PORT).expect("parses");
    let (_, diags) = check(&doc).expect_err("no unconditional port exists");
    let rendered = diags.render(SCOPED_PORT);
    assert!(
        rendered.contains("`arg X`: service `worker` has no unconditional `port`"),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "next: declare `port 3000` unconditionally inside `service worker`, or use `worker.name`"
        ),
        "{rendered}"
    );
}

const SCOPED_SERVICE: &str = r#"# forkfile: 0.1

image api . {
  arg P = svc.port

  on dev {
    service svc {
      port 9000
      route /
      run: app
    }
  }
}
"#;

#[test]
fn a_service_declared_in_an_on_branch_still_binds_for_every_feature_set() {
    let doc = parse(SCOPED_SERVICE).expect("parses");
    check(&doc).expect("the flat namespace holds `svc` in every branch");
    let dev = doc.default_features();
    let prod = doc.resolve_features(true, &[]).expect("valid features");
    assert_eq!(
        doc.eval_image_args_with("acme", "http", "api", &dev)["P"],
        "9000"
    );
    assert_eq!(
        doc.eval_image_args_with("acme", "http", "api", &prod)["P"],
        "9000",
        "a feature-scoped service is never a silent drop"
    );
}

const SCOPED_RESOURCE: &str = r#"# forkfile: 0.1

on dev {
  postgres pg { database app_db }
}

image api . {
  arg DB = app_db.url
}
"#;

#[test]
fn a_resource_declared_in_an_on_branch_still_binds_for_every_feature_set() {
    let doc = parse(SCOPED_RESOURCE).expect("parses");
    check(&doc).expect("the flat namespace holds `app_db` in every branch");
    let prod = doc.resolve_features(true, &[]).expect("valid features");
    assert_eq!(
        doc.eval_image_args_with("acme", "http", "api", &prod)["DB"],
        "postgresql://postgres@127.0.0.1:5432/app_db",
        "a feature-scoped resource is never a silent drop"
    );
}

const DEV_ARG: &str = r#"# forkfile: 0.1

image api . {
  on dev { arg DEBUG = "1" }

  service web {
    port 3000
    route /
    run: app
  }
}
"#;

#[test]
fn a_feature_scoped_arg_exists_only_when_its_feature_is_active() {
    let doc = parse(DEV_ARG).expect("parses");
    let dev = doc.default_features();
    assert!(dev.contains("dev"), "`dev` is built in");
    let without = doc
        .resolve_features(true, &[])
        .expect("no features is a valid set");
    assert!(
        doc.eval_image_args_with("acme", "http", "api", &dev)
            .contains_key("DEBUG")
    );
    assert!(
        !doc.eval_image_args_with("acme", "http", "api", &without)
            .contains_key("DEBUG")
    );
}

const RUNTIME_VAR: &str = r#"# forkfile: 0.1

image api . {
  arg A = var(KEY)
}
"#;

#[test]
fn a_runtime_var_is_refused_in_a_build_time_arg() {
    let doc = parse(RUNTIME_VAR).expect("parses");
    let (_, diags) = check(&doc).expect_err("`var(KEY)` is runtime-only");
    let rendered = diags.render(RUNTIME_VAR);
    assert!(
        rendered.contains(
            "`arg A`: `var(KEY)` exists only at runtime, so a build-time `arg` cannot read it"
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains("next: move it into `env { ... }`"),
        "{rendered}"
    );
}

const FEATURE_ARG: &str = r#"# forkfile: 0.1

features {
  mail
}

image api . {
  arg A = dev & mail
}
"#;

#[test]
fn a_feature_condition_is_refused_as_an_arg_value() {
    let doc = parse(FEATURE_ARG).expect("parses");
    let (_, diags) = check(&doc).expect_err("a feature condition has no build value");
    let rendered = diags.render(FEATURE_ARG);
    assert!(
        rendered.contains("`arg A`: a build-time `arg` needs a value, not a feature condition"),
        "{rendered}"
    );
    assert!(
        rendered.contains("next: select the value per feature with `on dev: arg PORT = \"5173\"`"),
        "{rendered}"
    );
}

const REUSED: &str = r#"# forkfile: 0.1

image front . {
  arg PORT = front.port

  service front {
    port 4173
    route /
    run: preview --port $port
    on dev { port 5173 }
  }
}
"#;

#[test]
fn a_service_reusing_its_image_slot_binds_its_really_running_port() {
    let doc = parse(REUSED).expect("parses");
    let checked = check(&doc).expect("the reused slot is a service symbol");
    let symbol = checked.symbols.get("front").expect("`front` is declared");
    assert_eq!(symbol.kind, crate::check::SymbolKind::Service);
    assert_eq!(
        symbol.port,
        Some(4173),
        "only the unconditional port is the symbol's"
    );
    let prod = doc.resolve_features(true, &[]).expect("valid features");
    let dev = doc.default_features();
    assert_eq!(
        doc.eval_image_args_with("acme", "http", "front", &prod)["PORT"],
        "4173",
        "an inactive `on` branch never overrides the port"
    );
    assert_eq!(
        doc.eval_image_args_with("acme", "http", "front", &dev)["PORT"],
        "5173"
    );
}
