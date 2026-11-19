//! The seeded `support/demo` manifest is a real, parseable world: two services and one
//! postgres cluster behind one origin.

use std::path::Path;

use slipfork_forkfile::{
    ArgPart, ArgRef, ArgRefKind, Argv, FeatureSet, FileVaultProvider, HookPhase, OsCsprngEntropy,
    Resolvers, parse_forkfile,
};

/// The demo ships as user-facing proof, so its `Forkfile` is part of the contract.
const DEMO: &str = include_str!("../../../support/demo/Forkfile");

/// Every path the manifest references must exist in the seeded demo directory.
const REFERENCED_PATHS: [&str; 8] = [
    "api/Dockerfile",
    "api/server.py",
    "api/migrate.py",
    "front/Dockerfile",
    "front/serve.py",
    "front/index.html",
    "db/0001_schema.sql",
    "db/0002_seed.sql",
];

fn render(argv: &Argv, port: u32) -> Vec<String> {
    argv.slots
        .iter()
        .map(|slot| {
            slot.parts
                .iter()
                .map(|part| match part {
                    ArgPart::Text(text) => text.clone(),
                    ArgPart::Ref(ArgRef {
                        kind: ArgRefKind::Port,
                        ..
                    }) => port.to_string(),
                    ArgPart::Ref(other) => panic!("unexpected reference in the demo: {other:?}"),
                })
                .collect::<String>()
        })
        .collect()
}

#[test]
fn every_referenced_demo_path_exists() {
    let demo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../support/demo");
    for relative in REFERENCED_PATHS {
        let path = demo.join(relative);
        assert!(path.is_file(), "support/demo/{relative} must exist");
    }
}

#[test]
fn demo_forkfile_parses_and_evaluates_three_containers() {
    let fork = parse_forkfile(DEMO).expect("support/demo/Forkfile must parse");
    let evaluated = fork
        .evaluate(&FeatureSet::new())
        .expect("support/demo/Forkfile must evaluate");
    assert_eq!(evaluated.project.as_deref(), Some("demo"));

    let services: Vec<&str> = evaluated.services.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(services, ["api", "front"]);

    let api = &evaluated.services[0];
    assert_eq!(api.port, Some(8000));
    assert_eq!(api.routes, ["/api"]);
    assert_eq!(api.volumes, [".venv"]);
    let api_run = api.run.as_ref().expect("api `run`");
    assert_eq!(
        render(api_run, 8000),
        [".venv/bin/python", "server.py", "8000"],
        "the api argv expands `$port` into the declared port"
    );

    assert_eq!(api.hooks.len(), 2, "setup + pre-deploy");
    assert_eq!(api.hooks[0].phase, HookPhase::Setup);
    assert_eq!(
        render(&api.hooks[0].argv, 8000),
        ["python3", "-m", "venv", "--system-site-packages", ".venv"]
    );
    assert_eq!(api.hooks[1].phase, HookPhase::PreDeploy);
    assert_eq!(
        render(&api.hooks[1].argv, 8000),
        [".venv/bin/python", "migrate.py"]
    );

    let front = &evaluated.services[1];
    assert_eq!(front.port, Some(5173));
    assert_eq!(front.routes, ["/"]);
    let front_run = front.run.as_ref().expect("front `run`");
    assert_eq!(
        render(front_run, 5173),
        ["python3", "serve.py", "5173"],
        "the front argv expands `$port` into the declared port"
    );

    assert_eq!(evaluated.postgres.len(), 1);
    let pg = &evaluated.postgres[0];
    assert_eq!(pg.id, "pg");
    assert_eq!(pg.version, "18");
    assert_eq!(pg.databases.len(), 1);
    assert_eq!(pg.databases[0].id, "demo");

    assert_eq!(evaluated.tasks.len(), 1);
    assert_eq!(evaluated.tasks[0].name, "migrate");
    assert_eq!(evaluated.tasks[0].service, "api");

    // Two services plus the postgres cluster: three containers behind one origin.
    assert_eq!(evaluated.services.len() + evaluated.postgres.len(), 3);
}

#[test]
fn demo_env_resolves_the_database_url_without_secret_taint() {
    let fork = parse_forkfile(DEMO).expect("support/demo/Forkfile must parse");
    let evaluated = fork
        .evaluate(&FeatureSet::new())
        .expect("support/demo/Forkfile must evaluate");

    let dir = tempfile::Builder::new()
        .prefix("demo-forkfile")
        .tempdir_in("/var/tmp")
        .expect("disk scratch");
    let vault = FileVaultProvider::open(dir.path().join("vault.json")).expect("open vault");
    let resolvers = Resolvers::new(&vault, &OsCsprngEntropy);
    let env = evaluated
        .resolve_env("api", &resolvers)
        .expect("demo env resolves");

    let url = &env["DATABASE_URL"];
    assert_eq!(
        url.expose(),
        "postgres://slipfork@127.0.0.1:5432/demo",
        "`DATABASE_URL = demo.url` resolves through the pg cluster's database"
    );
    assert!(
        !url.is_tainted(),
        "a passwordless coordinate is not a secret"
    );

    let generated = &env["DEMO_SECRET"];
    assert_eq!(generated.expose().len(), 32, "random_hex(16) is 16 bytes");
    assert!(generated.is_tainted(), "a generated secret stays secret");
}
