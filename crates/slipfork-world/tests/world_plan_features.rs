//! Proves `world_plan`/`world_plan_with` only see declarations the active feature set enables.

use std::fs;
use std::path::{Path, PathBuf};

use slipfork_forkfile::FeatureSet;
use slipfork_world::{WorldPlan, world_plan, world_plan_with};

const WITH_MAIL: &str = r#"# forkfile: 0.1
features {
  dev
  mail
  default = [dev, mail]
}
image app . {
  dockerfile Dockerfile
  on mail {
    service guarded {
      run: sh -c true
    }
  }
  service always {
    run: sh -c true
  }
}
"#;

const WITHOUT_MAIL: &str = r#"# forkfile: 0.1
features {
  dev
  mail
  default = [dev]
}
image app . {
  dockerfile Dockerfile
  on mail {
    service guarded {
      run: sh -c true
    }
  }
  service always {
    run: sh -c true
  }
}
"#;

const DEV_GATE: &str = r#"# forkfile: 0.1
features {
  dev
}
image app . {
  dockerfile Dockerfile
  on dev {
    service guarded {
      run: sh -c true
    }
  }
  service always {
    run: sh -c true
  }
}
"#;

fn lab_dir(name: &str) -> PathBuf {
    let dir =
        Path::new("/var/tmp").join(format!("slipfork-world-plan-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create lab dir");
    dir
}

fn plan(name: &str, source: &str) -> WorldPlan {
    let dir = lab_dir(name);
    fs::write(dir.join("Dockerfile"), "FROM scratch\n").expect("write Dockerfile");
    let document = slipfork_forkfile::parse(source).expect("the feature fixture parses");
    slipfork_forkfile::check(&document).expect("the feature fixture validates");
    world_plan(&document, &dir).expect("the world plan builds")
}

fn plan_with(name: &str, source: &str, no_default: bool) -> WorldPlan {
    let dir = lab_dir(name);
    fs::write(dir.join("Dockerfile"), "FROM scratch\n").expect("write Dockerfile");
    let document = slipfork_forkfile::parse(source).expect("the feature fixture parses");
    slipfork_forkfile::check(&document).expect("the feature fixture validates");
    let features: FeatureSet = document
        .resolve_features(no_default, &[])
        .expect("the feature fixture resolves");
    world_plan_with(&document, &dir, &features).expect("the world plan builds")
}

fn service_ids(plan: &WorldPlan) -> Vec<&str> {
    plan.services
        .iter()
        .map(|service| service.id.as_str())
        .collect()
}

#[test]
fn on_mail_services_appear_only_when_mail_is_active() {
    let enabled = plan("enabled", WITH_MAIL);
    assert_eq!(
        service_ids(&enabled),
        ["guarded", "always"],
        "`on mail` expands when `mail` is in default"
    );

    let disabled = plan("disabled", WITHOUT_MAIL);
    assert_eq!(
        service_ids(&disabled),
        ["always"],
        "`on mail` stays out while mail is inactive"
    );
}

#[test]
fn no_default_features_drops_the_built_in_dev_gate() {
    let defaulted = plan("dev-default", DEV_GATE);
    assert_eq!(service_ids(&defaulted), ["guarded", "always"]);

    let bare = plan_with("dev-bare", DEV_GATE, true);
    assert_eq!(
        service_ids(&bare),
        ["always"],
        "`--no-default-features` turns the built-in `dev` gate off"
    );
}

const S3_PUBLIC_READ: &str = r#"# forkfile: 0.1
s3 docs {
  policy public-read
}
"#;

#[test]
fn declared_s3_policy_reaches_the_world_plan() {
    let planned = plan("s3-policy", S3_PUBLIC_READ);
    let buckets: Vec<&slipfork_world::S3Bucket> = planned
        .resources
        .iter()
        .filter(|node| node.resource == slipfork_world::ResourceKind::S3)
        .flat_map(|node| node.buckets.iter())
        .collect();
    assert_eq!(buckets.len(), 1, "one bucket resource");
    assert_eq!(buckets[0].name(), "docs");
    assert_eq!(
        buckets[0].policy(),
        slipfork_forkfile::BucketPolicy::PublicRead,
        "`policy public-read` without `cdn` must stay public-read in the boot plan"
    );
}
