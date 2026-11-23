const SOURCE: &str = r#"# forkfile: 0.1

env base {
  LOG_LEVEL = "info"
  SECRET_KEY = secret(SECRET_KEY)
  TURN = 2
}

image app . {
  use env base
  env {
    EXTRA = true
  }
  service api {
    route /
  }
  on dev {
    service worker {
      run: serve
    }
  }
}
"#;

#[test]
fn env_bindings_flatten_groups_images_services_and_on_blocks() {
    let document = slipfork_forkfile_parse(SOURCE);
    let bindings = document.env_bindings();

    assert!(bindings["base"]["SECRET_KEY"].is_secret());
    assert_eq!(
        bindings["base"]["LOG_LEVEL"].literal.as_deref(),
        Some("info")
    );
    assert_eq!(bindings["base"]["TURN"].literal.as_deref(), Some("2"));

    // The image and both of its services inherit the group plus the inline block.
    for component in ["app", "api", "worker"] {
        assert_eq!(
            bindings[component]["LOG_LEVEL"].literal.as_deref(),
            Some("info"),
            "{component} must inherit `use env base`"
        );
        assert_eq!(
            bindings[component]["TURN"].literal.as_deref(),
            Some("2"),
            "{component}"
        );
        assert_eq!(
            bindings[component]["EXTRA"].literal.as_deref(),
            Some("true"),
            "{component} must inherit the inline env block"
        );
        assert!(bindings[component]["SECRET_KEY"].is_secret(), "{component}");
    }
}

fn slipfork_forkfile_parse(source: &str) -> crate::ast::Forkfile {
    crate::parse(source).expect("the visitor fixture must be a valid Forkfile")
}

#[test]
fn services_name_their_enclosing_image_and_postgres_owns_its_databases() {
    let document = slipfork_forkfile_parse(
        "# forkfile: 0.1\n\
             postgres pg { database app-db }\n\
             image app . {\n\
             \x20 service api { port 8000 }\n\
             }\n",
    );
    let services = document.services();
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].id, "api");
    assert_eq!(services[0].image, Some("app"));
    let resources = document.resources();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].id, "pg");
    assert_eq!(resources[0].databases, vec!["app-db"]);
}
