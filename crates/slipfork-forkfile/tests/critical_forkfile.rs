//! Protected end-to-end invariants for `Forkfile` semantics and migration.

use std::fs;
use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use slipfork_forkfile::{
    BucketPolicy, DnsLabel, GeneratorSpec, Resolvers, SecretValue, boot_plan, check, format_check,
    migrate, parse,
};

fn eval_env(
    document: &slipfork_forkfile::Forkfile,
) -> BTreeMap<String, BTreeMap<String, (String, bool)>> {
    let secret = |_: &str, _: &str, _: Option<&GeneratorSpec>| None;
    let var = |_: &str, _: Option<&GeneratorSpec>| None;
    document.eval_all_env_with(
        "demo",
        "http",
        &Resolvers {
            secret: &secret,
            var: &var,
        },
    )
}

const SNOW: &str = r#"images:
  - id: backend
    context: .
    dockerfile: Dockerfile
    envs: [common]
  - id: frontend
    context: ./front
resources:
  pg_clusters:
    - id: pg
      version: 16
  pg_databases:
    - id: app_db
      cluster: pg
  redis_clusters:
    - id: cache
  s3_buckets:
    - uploads
    - id: assets
domains:
  - main
envs:
  common:
    DATABASE_URL: "postgres://${{ resources::pg_databases.app_db.url }}"
    REDIS_URL: "${{ resources::redis_clusters.cache.url }}"
    PUBLIC_URL: "${{ domains::main }}"
    SECRET_KEY: "${{ secrets::SECRET_KEY ?? random_hex(32) }}"
    COMMIT: "${{ deployment::domain }}"
components:
  - id: api
    image: backend
    run_command: gunicorn app.main:app
  - id: worker
    image: backend
    run_command: celery -A app.worker worker
  - id: front
    image: frontend
    run_command: node server.js
ingresses:
  - match: { prefix: /api }
    target: { component: api, port: 8000 }
  - match: { prefix: / }
    target: { component: front, port: 4173 }
hooks:
  - component: api
    when: before_start
    run_command: alembic upgrade head
modes:
  dev:
    envs:
      dev:
        DEBUG: "1"
    components:
      front:
        envs: [common]
"#;

const MIGRATED: &str = "# forkfile: 0.1
# migrated from snow.yml
features {
  dev
}

postgres pg {
  version 16
  database app_db
}

redis cache

s3 uploads

s3 assets

domain main

env common {
  COMMIT = self.domain
  DATABASE_URL = \"postgres://\" + app_db.url
  PUBLIC_URL = main.name
  REDIS_URL = cache.url
  SECRET_KEY = secret(SECRET_KEY, random_hex(32))
}

on dev {
  env dev {
    DEBUG = \"1\"
  }
}

image backend . {
  dockerfile Dockerfile
  use env common
  service api {
    port 8000
    route /api
    before-start: alembic upgrade head
    run: gunicorn app.main:app
  }
  service worker {
    run: celery -A app.worker worker
  }
}

image frontend ./front {
  on dev: use env common
  service front {
    port 4173
    route /
    run: node server.js
  }
}
";

fn lab_dir(name: &str) -> PathBuf {
    let dir =
        Path::new("/var/tmp").join(format!("slipfork-forkfile-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create lab dir");
    dir
}

#[test]
fn build_time_arg_rejects_secret_native_taint() {
    let source = "# forkfile: 0.1\n\npostgres pg { database app_db }\n\nimage backend . {\n  arg TOKEN = secret(TOKEN)\n  arg PASSWORD = app_db.password\n  env {\n    TOKEN = secret(TOKEN)\n  }\n}\n";
    let document = parse(source).expect("parses");
    let (_, diags) = check(&document).expect_err("tainted args are refused");
    assert!(
        diags
            .items
            .iter()
            .any(|d| d.message.contains("bake a secret")),
        "{diags:?}"
    );
    assert_eq!(
        diags
            .items
            .iter()
            .filter(|d| d.message.contains("bake a secret"))
            .count(),
        2,
        "{diags:?}"
    );
}

#[test]
fn flat_namespace_duplicates_name_both_kinds_and_spans() {
    let source = "# forkfile: 0.1\n\ndomain cache\nredis cache\n";
    let (_, diags) = check(&parse(source).expect("parses")).expect_err("duplicate symbols");
    let first = diags.first().expect("one diagnostic");
    assert!(first.message.contains("duplicate symbol `cache`"));
    assert!(first.message.contains("`domain`"));
    assert!(first.message.contains("`redis`"));
    assert!(first.message.contains("Forkfile:3:8"), "{}", first.message);
}

#[test]
fn every_on_branch_is_typed_even_when_not_selected() {
    let source = "# forkfile: 0.1\n\nfeatures {\n  default = [dev]\n  dev\n}\n\nimage backend . {\n  on !dev: arg TOKEN = secret(TOKEN)\n}\n";
    let (_, diags) = check(&parse(source).expect("parses")).expect_err("branch is typed");
    assert!(
        diags
            .items
            .iter()
            .any(|d| d.message.contains("bake a secret")),
        "{diags:?}"
    );
}

#[test]
fn duplicate_routes_in_one_branch_are_rejected() {
    let source = "# forkfile: 0.1\n\nimage backend . {\n  service api {\n    port 8000\n    route /api\n    route /api\n  }\n}\n";
    let (_, diags) = check(&parse(source).expect("parses")).expect_err("route conflict");
    assert!(
        diags
            .items
            .iter()
            .any(|d| d.message.contains("duplicate `route`")),
        "{diags:?}"
    );
}

#[test]
fn cdn_requires_explicit_public_read_with_a_caret_and_remedy() {
    let source = "# forkfile: 0.1\n\ns3 assets { cdn static-cdn }\n";
    let (_, diags) = check(&parse(source).expect("parses")).expect_err("cdn needs public-read");
    let rendered = diags.render(source);
    let expected = "Forkfile:3:13\ns3 assets { cdn static-cdn }\n            ^\n'cdn static-cdn' inside 's3 assets' requires 'policy public-read'\nnext: add 'policy public-read' inside 's3 assets' to enable 'cdn static-cdn'";
    assert_eq!(rendered, expected);

    let guarded = "# forkfile: 0.1\n\nfeatures {\n  dev\n}\n\ns3 assets {\n  on dev: policy public-read\n  on dev: cdn static-cdn\n}\n";
    check(&parse(guarded).expect("parses")).expect("a branch policy legitimises its cdn");

    let unselected = "# forkfile: 0.1\n\nfeatures {\n  dev\n  staging\n}\n\ns3 assets {\n  on staging: policy public-read\n  cdn static-cdn\n}\n";
    let (_, diags) = check(&parse(unselected).expect("parses")).expect_err("unselected policy");
    assert!(
        diags
            .items
            .iter()
            .any(|diagnostic| diagnostic.message.contains("requires 'policy public-read'")),
        "{diags:?}"
    );

    let mut features = String::from("# forkfile: 0.1\n\nfeatures {\n");
    for index in 1..=13 {
        features.push_str(&format!("  f{index}\n"));
    }
    features.push_str("}\n\ns3 assets {\n  on f1: cdn static-cdn\n}\n");
    let (_, diags) = check(&parse(&features).expect("parses")).expect_err("13-feature branch");
    assert!(
        diags
            .items
            .iter()
            .any(|diagnostic| diagnostic.message.contains("requires 'policy public-read'")),
        "{diags:?}"
    );
}

#[test]
fn policy_parses_formats_canonically_and_round_trips() {
    let source = "# forkfile: 0.1\n\ns3 assets {\n  cdn static-cdn\n  policy public-read\n}\n";
    let document = parse(source).expect("parses");
    let formatted = slipfork_forkfile::format(&document);
    let expected = "# forkfile: 0.1\ns3 assets {\n  policy public-read\n  cdn static-cdn\n}\n";
    assert_eq!(formatted, expected);
    let reparsed = parse(&formatted).expect("reparses");
    assert_eq!(slipfork_forkfile::format(&reparsed), formatted);
    let bucket = match &reparsed.items.items[0] {
        slipfork_forkfile::TopItem::S3(bucket) => bucket,
        _ => panic!("expected an s3 bucket"),
    };
    assert_eq!(
        bucket.policy_at(&std::collections::BTreeSet::new()),
        BucketPolicy::PublicRead
    );
}

#[test]
fn rfc1123_authorities_fail_closed_at_the_compiler() {
    for source in [
        "# forkfile: 0.1\n\ndomain bad_name\n",
        "# forkfile: 0.1\n\ndomain -cdn\n",
        "# forkfile: 0.1\n\ndomain cdn-\n",
        "# forkfile: 0.1\n\ns3 ab\n",
        "# forkfile: 0.1\n\ns3 1.2.3.4\n",
        "# forkfile: 0.1\n\ns3 assets {\n  policy public-read\n  cdn bad_name\n}\n",
    ] {
        let parsed = parse(source);
        let refused = match parsed {
            Err(_) => true,
            Ok(document) => check(&document).is_err(),
        };
        assert!(refused, "must reject:\n{source}");
    }
    assert!(DnsLabel::new("static-cdn").is_ok());
    assert!(DnsLabel::bucket("1.2.3.4").is_err());
}

#[test]
fn secret_values_redact_and_bucket_taint_follows_policy() {
    let secret = SecretValue::new("hunter2");
    assert_eq!(format!("{secret:?}"), "[REDACTED]");
    assert_eq!(secret.to_string(), "[REDACTED]");
    assert_eq!(secret.expose_secret(), "hunter2");

    let private = parse(
        "# forkfile: 0.1\n\ns3 uploads\n\nenv common {\n  URL = uploads.url\n  KEY = uploads.secret_key\n  ACCESS = uploads.access_key\n  ENDPOINT = uploads.endpoint\n}\n",
    )
    .expect("parses");
    let env = eval_env(&private);
    assert!(
        env["common"]["URL"].1,
        "private bucket url is secret-tainted"
    );
    assert!(env["common"]["KEY"].1, "secret_key is always tainted");
    assert!(!env["common"]["ACCESS"].1, "access_key is non-secret");
    assert!(!env["common"]["ENDPOINT"].1, "endpoint is non-secret");
    assert_eq!(env["common"]["URL"].0, "http://uploads.demo.localhost");

    let public = parse(
        "# forkfile: 0.1\n\ns3 uploads { policy public-read }\n\nenv common {\n  URL = uploads.url\n}\n",
    )
    .expect("parses");
    let env = eval_env(&public);
    assert!(!env["common"]["URL"].1, "public-read url is non-secret");
}

#[test]
fn migration_is_deterministic_lands_on_disk_and_typechecks() {
    let dir = lab_dir("migrate");
    let input = dir.join("snow.yml");
    let output = dir.join("Forkfile");
    fs::write(&input, SNOW).expect("write snow.yml");

    let first = migrate(&fs::read_to_string(&input).expect("read snow.yml")).expect("migrates");
    let second = migrate(&fs::read_to_string(&input).expect("read snow.yml")).expect("migrates");
    assert_eq!(first, second, "identical input must be byte-identical");
    assert_eq!(first, MIGRATED, "migration golden changed");

    let canonical = format_check(&first).expect("migrated output parses");
    assert!(canonical.is_formatted(), "{}", canonical.diff);

    fs::write(&output, &first).expect("write Forkfile");
    let on_disk = fs::read_to_string(&output).expect("read Forkfile");
    let document = parse(&on_disk).expect("migrated output parses");
    let checked = check(&document).expect("migrated output typechecks");
    assert_eq!(
        checked.features.into_iter().collect::<Vec<_>>(),
        vec!["dev".to_string()]
    );
    fs::remove_dir_all(&dir).expect("clean lab dir");
}

#[test]
fn migration_refuses_unknown_keys_and_disabled_component_overrides() {
    let unknown = migrate("images: []\nmystery: 1\n").expect_err("unknown key refused");
    let first = unknown.first().expect("one diagnostic");
    assert!(first.message.contains("mystery"));
    assert!(first.message.contains("images"));
    assert!(first.next.contains("`images`"));
    assert!(first.next.contains("`resources`"));
    assert_eq!(first.span.line(), 2);

    let disabled = "images:\n  - id: backend\n    context: .\ncomponents:\n  - id: api\n    image: backend\n    run_command: run\nmodes:\n  dev:\n    components:\n      api:\n        disabled: true\n";
    let diags = migrate(disabled).expect_err("disabled override refused");
    let first = diags.first().expect("one diagnostic");
    assert!(first.message.contains("modes.dev.components.api.disabled"));
    assert_eq!(first.span.line(), 12);
    assert!(first.next.contains("on <feature>"));

    let rendered = diags.render_as(disabled, "snow.yml");
    assert!(rendered.contains("snow.yml:12:"), "{rendered}");
    assert!(!rendered.contains("Forkfile:"), "{rendered}");
}

#[test]
fn migration_duplicate_symbols_embed_both_yaml_spans() {
    let source = "domains:\n  - cache\nresources:\n  redis_clusters:\n    - id: cache\n";
    let diags = migrate(source).expect_err("duplicate symbol");
    let first = diags.first().expect("one diagnostic");
    assert!(
        first.message.contains("duplicate symbol `cache`"),
        "{}",
        first.message
    );
    assert!(first.message.contains("snow.yml:2:"), "{}", first.message);
    assert!(first.message.contains("snow.yml:5:"), "{}", first.message);
}

#[test]
fn migration_refuses_shell_pipelines_and_unknown_template_namespaces() {
    let pipeline = "images:\n  - id: backend\n    context: .\ncomponents:\n  - id: api\n    image: backend\n    run_command: cat a | grep b\n";
    let diags = migrate(pipeline).expect_err("pipeline refused");
    assert!(
        diags
            .items
            .iter()
            .any(|d| d.next.contains("scripts/<name>.sh")),
        "{diags:?}"
    );

    let template = "envs:\n  common:\n    URL: \"${{ rollback::x }}\"\n";
    let diags = migrate(template).expect_err("namespace refused");
    assert!(
        diags
            .items
            .iter()
            .any(|d| d.message.contains("envs.common.URL")),
        "{diags:?}"
    );
}

#[test]
fn uv_prequels_dedupe_per_image_and_lockfile_dir() {
    let dir = lab_dir("prequel-dedupe");
    fs::write(dir.join("uv.lock"), "version = 1\n").expect("write lockfile");
    let source = "# forkfile: 0.1\n\nimage backend . {\n  service api {\n    run: @uv run app\n  }\n  service worker {\n    run: @uv run worker\n  }\n}\n";
    let plan = boot_plan(&parse(source).expect("parses"), &dir);
    assert_eq!(plan.len(), 1, "{plan:?}");
    let node = &plan.nodes[0];
    assert_eq!(node.image, "backend");
    assert_eq!(node.command, "uv sync");
    assert_eq!(node.directory, PathBuf::from("."));
    assert_eq!(node.patterns, vec![PathBuf::from("uv.lock")]);
    assert!(node.is_current(&dir));

    let source = "# forkfile: 0.1\n\nimage backend . {\n  service api {\n    run: @uv run app\n  }\n}\n\nimage front ./front {\n  service web {\n    run: @uv run dev\n  }\n}\n";
    fs::create_dir_all(dir.join("front")).expect("front directory");
    fs::write(dir.join("front/uv.lock"), "version = 1\n").expect("write front lockfile");
    let plan = boot_plan(&parse(source).expect("parses"), &dir);
    assert_eq!(plan.len(), 2, "{plan:?}");
    assert_eq!(plan.nodes[0].directory, PathBuf::from("."));
    assert_eq!(plan.nodes[1].directory, PathBuf::from("front"));
    fs::remove_dir_all(&dir).expect("clean lab dir");
}

#[test]
fn pnpm_prequel_walks_up_to_the_workspace_root_lockfile() {
    let dir = lab_dir("prequel-workspace");
    fs::write(dir.join("pnpm-lock.yaml"), "lockfileVersion: 9\n").expect("write lockfile");
    fs::create_dir_all(dir.join("apps/web")).expect("workspace directory");
    let source =
        "# forkfile: 0.1\n\nimage web ./apps/web {\n  service web {\n    run: @pnpm dev\n  }\n}\n";
    let plan = boot_plan(&parse(source).expect("parses"), &dir);
    assert_eq!(plan.len(), 1, "{plan:?}");
    let node = &plan.nodes[0];
    assert_eq!(node.command, "pnpm install");
    assert_eq!(node.directory, PathBuf::from("."));
    assert_eq!(node.patterns, vec![PathBuf::from("pnpm-lock.yaml")]);
    fs::remove_dir_all(&dir).expect("clean lab dir");
}

#[test]
fn when_changed_stamps_rerun_only_after_inputs_change() {
    let dir = lab_dir("prequel-stamp");
    fs::create_dir_all(dir.join("prisma")).expect("prisma directory");
    let schema = dir.join("prisma/schema.prisma");
    fs::write(&schema, "model User {}\n").expect("write schema");
    let source = "# forkfile: 0.1\n\nimage backend . {\n  service api {\n    before-start when changed(prisma/schema.prisma): alembic upgrade head\n    run: gunicorn app:app\n  }\n}\n";
    let plan = boot_plan(&parse(source).expect("parses"), &dir);
    assert_eq!(plan.len(), 1, "{plan:?}");
    let node = &plan.nodes[0];
    assert_eq!(node.patterns, vec![PathBuf::from("prisma/schema.prisma")]);
    assert!(node.is_current(&dir));
    fs::write(&schema, "model Org {}\n").expect("change schema");
    assert!(
        !node.is_current(&dir),
        "changed input must invalidate the stamp"
    );
    fs::write(&schema, "model User {}\n").expect("restore schema");
    assert!(node.is_current(&dir), "restored content restores the stamp");
    let again = boot_plan(&parse(source).expect("parses"), &dir);
    assert_eq!(plan, again);
    fs::remove_dir_all(&dir).expect("clean lab dir");
}

#[test]
fn unconditional_hooks_are_always_due_and_run_is_skipped() {
    let dir = lab_dir("prequel-unconditional");
    let source = "# forkfile: 0.1\n\nimage backend . {\n  service api {\n    before-start: alembic upgrade head\n    run: gunicorn app:app\n  }\n}\n";
    let plan = boot_plan(&parse(source).expect("parses"), &dir);
    assert_eq!(plan.len(), 1, "{plan:?}");
    let node = &plan.nodes[0];
    assert_eq!(node.command, "alembic upgrade head");
    assert!(node.patterns.is_empty(), "{node:?}");
    assert!(!node.is_current(&dir), "unconditional hooks are always due");
    fs::remove_dir_all(&dir).expect("clean lab dir");
}

#[test]
fn from_images_plan_against_the_project_root_and_missing_inputs_are_due() {
    let dir = lab_dir("prequel-from");
    let source = "# forkfile: 0.1\n\nimage base from python:3.12 {\n  service api {\n    before-start when changed(prisma/missing.prisma): alembic upgrade head\n    run: gunicorn app:app\n  }\n}\n";
    let plan = boot_plan(&parse(source).expect("parses"), &dir);
    assert_eq!(plan.len(), 1, "{plan:?}");
    let node = &plan.nodes[0];
    assert_eq!(node.directory, PathBuf::from("."));
    assert_eq!(node.patterns, vec![PathBuf::from("prisma/missing.prisma")]);
    assert!(
        !node.is_current(&dir),
        "a missing input can never be current"
    );
    fs::remove_dir_all(&dir).expect("clean lab dir");
}

#[test]
fn same_command_with_different_watched_inputs_stays_two_nodes() {
    let dir = lab_dir("prequel-watched");
    fs::create_dir_all(dir.join("a")).expect("a directory");
    fs::create_dir_all(dir.join("b")).expect("b directory");
    fs::write(dir.join("a/one.txt"), "1\n").expect("first input");
    fs::write(dir.join("b/two.txt"), "2\n").expect("second input");
    let source = "# forkfile: 0.1\n\nimage backend . {\n  service api {\n    before-start when changed(a/one.txt): make generate\n    before-start when changed(b/two.txt): make generate\n  }\n}\n";
    let plan = boot_plan(&parse(source).expect("parses"), &dir);
    assert_eq!(plan.len(), 2, "{plan:?}");
    assert_eq!(plan.nodes[0].patterns, vec![PathBuf::from("a/one.txt")]);
    assert_eq!(plan.nodes[1].patterns, vec![PathBuf::from("b/two.txt")]);
    assert!(plan.nodes[0].is_current(&dir));
    assert!(plan.nodes[1].is_current(&dir));
    fs::remove_dir_all(&dir).expect("clean lab dir");
}

#[test]
fn interpolated_strings_typecheck_evaluate_and_round_trip() {
    let source = "# forkfile: 0.1\n\ndomain main\ndomain dom-b\n\nenv base {\n  LOGIN = \"${self.url}/login\"\n  CALLBACK = \"${self.scheme}://${dom-b.name}/cb\"\n  ESCAPED = \"literal \\${not_an_expr}\"\n  SINGLE = '${also_literal}'\n}\n";
    let document = parse(source).expect("interpolation parses");
    check(&document).expect("interpolation typechecks");
    let env = eval_env(&document);
    assert_eq!(env["base"]["LOGIN"].0, "http://demo.localhost/login");
    assert_eq!(env["base"]["CALLBACK"].0, "http://dom-b.demo.localhost/cb");
    assert_eq!(env["base"]["ESCAPED"].0, "literal ${not_an_expr}");
    assert_eq!(env["base"]["SINGLE"].0, "${also_literal}");

    let formatted = slipfork_forkfile::format(&document);
    assert!(formatted.contains("${self.url}/login"), "{formatted}");
    let reparsed = parse(&formatted).expect("formatted interpolation reparses");
    assert_eq!(document, reparsed);
    assert_eq!(formatted, slipfork_forkfile::format(&reparsed));
}

#[test]
fn interpolated_secret_taint_is_rejected_in_build_args() {
    let source = "# forkfile: 0.1\n\nimage backend . {\n  arg TOKEN = \"${secret(TOKEN)}\"\n}\n";
    let (_, diags) = check(&parse(source).expect("parses")).expect_err("tainted arg");
    assert!(
        diags
            .items
            .iter()
            .any(|d| d.message.contains("bake a secret")),
        "{diags:?}"
    );
}

#[test]
fn domains_expose_name_only_and_self_is_reserved() {
    let source = "# forkfile: 0.1\n\ndomain main\n\nenv base {\n  BAD = main.url\n}\n";
    let (_, diags) = check(&parse(source).expect("parses")).expect_err("domain url refused");
    let diagnostic = diags
        .items
        .iter()
        .find(|d| d.message.contains("not a domain property"))
        .expect("domain diagnostic");
    assert_eq!(
        diagnostic.next,
        "a domain is a bare hostname without a scheme; use 'main.name' for the hostname, 'self.url' for the app's primary URL, or '\"${self.scheme}://${main.name}\"' for an alternate domain's URL"
    );

    for source in [
        "# forkfile: 0.1\n\ndomain self\n",
        "# forkfile: 0.1\n\nimage self . {\n}\n",
        "# forkfile: 0.1\n\nimage a . {\n  service self {\n  }\n}\n",
    ] {
        let (_, diags) = check(&parse(source).expect("parses")).expect_err("self reserved");
        assert!(
            diags
                .items
                .iter()
                .any(|d| d.message.contains("`self` is reserved")),
            "{diags:?}"
        );
    }
}

#[test]
fn generators_are_only_the_second_argument_and_never_a_fallback() {
    let standalone = "# forkfile: 0.1\n\nenv base {\n  TOKEN = random_string(8)\n}\n";
    let (_, diags) = check(&parse(standalone).expect("parses")).expect_err("standalone refused");
    let diagnostic = diags.first().expect("one diagnostic");
    assert_eq!(
        diagnostic.next,
        "pass the generator as the second argument: secret(KEY, random_string(50))"
    );

    let fallback = "# forkfile: 0.1\n\nenv base {\n  TOKEN = secret(TOKEN) ?? random_hex(32)\n}\n";
    let (_, diags) = check(&parse(fallback).expect("parses")).expect_err("fallback refused");
    assert!(
        diags.items.iter().any(|d| d
            .message
            .contains("function calls are not allowed as `??` defaults")),
        "{diags:?}"
    );

    let out_of_range =
        "# forkfile: 0.1\n\nenv base {\n  TOKEN = secret(TOKEN, random_string(0))\n}\n";
    let diagnostics = parse(out_of_range).expect_err("length refused");
    assert!(
        diagnostics
            .items
            .iter()
            .any(|d| d.message.contains("out of range")),
        "{diagnostics:?}"
    );

    let generated = "# forkfile: 0.1\n\nenv base {\n  TOKEN = secret(TOKEN, random_string(50))\n  VAR = var(VAR, random_uuid())\n}\n";
    let document = parse(generated).expect("generators parse");
    check(&document).expect("generators typecheck");
    let formatted = slipfork_forkfile::format(&document);
    assert!(
        formatted.contains("secret(TOKEN, random_string(50))"),
        "{formatted}"
    );
    assert!(formatted.contains("var(VAR, random_uuid())"), "{formatted}");
}

#[test]
fn on_else_selects_the_false_branch_and_formats_canonically() {
    let source = "# forkfile: 0.1\n\nfeatures {\n  default = [dev]\n  dev\n}\n\nenv base {\n  on dev: VALUE = \"dev\" else: VALUE = \"prod\"\n}\n";
    let document = parse(source).expect("else parses");
    check(&document).expect("else typechecks");
    let dev = eval_env(&document);
    assert_eq!(dev["base"]["VALUE"].0, "dev");

    let formatted = slipfork_forkfile::format(&document);
    assert!(
        formatted.contains("on dev {\n    VALUE = \"dev\"\n  } else: VALUE = \"prod\""),
        "inline `on` + else must render the canonical block form:\n{formatted}"
    );
    let reparsed = parse(&formatted).expect("formatted else reparses");
    assert_eq!(eval_env(&reparsed)["base"]["VALUE"].0, "dev");
    assert_eq!(
        formatted,
        slipfork_forkfile::format(&reparsed),
        "formatted else output must be idempotent"
    );

    let blocked = "# forkfile: 0.1\n\nfeatures {\n  dev\n}\n\nimage backend . {\n  service api {\n    run {\n      on dev {\n        echo dev\n      } else {\n        echo prod\n      }\n    }\n  }\n}\n";
    let document = parse(blocked).expect("block else parses");
    check(&document).expect("block else typechecks");
    let formatted = slipfork_forkfile::format(&document);
    assert!(formatted.contains("} else {"), "{formatted}");
    assert_eq!(document, parse(&formatted).expect("reparses"));
}

#[test]
fn chained_else_on_picks_the_first_true_branch_and_formats_canonically() {
    let source = "# forkfile: 0.1\n\nfeatures {\n  default = [dev]\n  dev\n  prod\n}\n\nimage backend . {\n  service api {\n    run {\n      on dev {\n        echo dev\n      } else on prod {\n        echo prod\n      } else {\n        echo fallback\n      }\n    }\n  }\n}\n";
    let document = parse(source).expect("chained else parses");
    check(&document).expect("chained else typechecks");
    let image = document
        .items
        .items
        .iter()
        .find_map(|item| match item {
            slipfork_forkfile::TopItem::Image(image) => Some(image),
            _ => None,
        })
        .expect("image");
    let service = image
        .body
        .items
        .iter()
        .find_map(|item| match item {
            slipfork_forkfile::ImageItem::Service(service) => Some(service),
            _ => None,
        })
        .expect("service");
    let hook = service
        .body
        .items
        .iter()
        .find_map(|item| match item {
            slipfork_forkfile::ServiceItem::Run(hook) => Some(hook),
            _ => None,
        })
        .expect("run");
    let features = document.active_features();
    let texts: Vec<String> = hook
        .commands(&features)
        .iter()
        .map(|command| command.text())
        .collect();
    assert_eq!(texts, ["echo dev"]);

    let formatted = slipfork_forkfile::format(&document);
    assert!(formatted.contains("} else on prod {"), "{formatted}");
    assert_eq!(document, parse(&formatted).expect("reparses"));
}

#[test]
fn no_else_branch_is_selected_when_the_condition_is_false() {
    let source = "# forkfile: 0.1\n\nfeatures {\n  dev\n}\n\nenv base {\n  X = \"1\"\n  on dev: Y = \"2\"\n}\n";
    let document = parse(source).expect("parses");
    let env = eval_env(&document);
    assert!(!env["base"].contains_key("Y"));
}

#[test]
fn migration_refuses_an_unknown_deployment_property() {
    let source = "envs:\n  common:\n    COMMIT: \"${{ deployment::commit_sha }}\"\n";
    let diags = migrate(source).expect_err("unknown deployment property refused");
    let first = diags.first().expect("one diagnostic");
    assert!(first.message.contains("commit_sha"), "{}", first.message);
    assert!(
        first.next.contains("self.url") && first.next.contains("self.domain"),
        "{}",
        first.next
    );
}
