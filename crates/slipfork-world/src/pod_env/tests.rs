use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use tempfile::TempDir;

use super::*;
use crate::pod::{PodEnv, write_bundle};

fn host(uid: u32, gid: u32) -> PodHost {
    PodHost {
        crun: "/usr/bin/crun".into(),
        resolv_conf: None,
        uid,
        gid,
    }
}

fn spec(rootfs: PathBuf, inherited_env: Vec<String>, env: Vec<PodEnv>) -> PodSpec {
    PodSpec {
        id: "api".to_owned(),
        rootfs,
        workdir: "/project".to_owned(),
        argv: vec!["true".to_owned()],
        inherited_env,
        env,
        binds: Vec::new(),
        files: Vec::new(),
        identity: None,
    }
}

fn env_value(env: &[String], key: &str) -> Option<String> {
    env.iter()
        .find_map(|pair| pair.strip_prefix(&format!("{key}=")))
        .map(str::to_owned)
}

fn seeded_image(disk: &TempDir, passwd: &str, group: &str) -> PathBuf {
    let rootfs = disk.path().join("rootfs");
    fs::create_dir_all(rootfs.join("etc")).expect("seed the image etc");
    fs::write(rootfs.join("etc/passwd"), passwd).expect("write passwd");
    fs::write(rootfs.join("etc/group"), group).expect("write group");
    rootfs
}

#[test]
fn passwd_and_group_describe_the_effective_identity_without_shadowing_the_image() {
    let account = Account {
        user: "slipfork".to_owned(),
        group: "slipfork".to_owned(),
        home: POD_HOME.to_owned(),
        shell: "/bin/sh".to_owned(),
    };
    let passwd = passwd_contents(&account, 1000, 1001);
    assert!(passwd.contains("slipfork:x:1000:1001:slipfork:/home/slipfork:/bin/sh\n"));
    assert!(passwd.contains("root:x:0:0:root:/root:/sbin/nologin\n"));
    assert_eq!(
        group_contents(&account, 1001),
        "root:x:0:\nslipfork:x:1001\n"
    );

    let root = fallback(Identity::ROOT);
    assert!(passwd_contents(&root, 0, 0).contains("root:x:0:0:root:/root:/sbin/nologin\n"));
    assert_eq!(group_contents(&root, 0), "root:x:0\n");
}

#[test]
fn colour_hints_are_defaulted_while_explicit_values_and_no_color_win() {
    let probe = TempDir::new_in("/var/tmp").expect("real disk for the rootfs probe");
    let owner = fs::metadata(probe.path()).expect("stat the probe dir");
    let host = host(owner.uid(), owner.gid());
    let env_of = |unset: PodSpec| {
        let pod = PodIdentityMapping::resolve(&unset, &host);
        merged_env(&unset, &pod, host.uid)
    };

    let plain = env_of(spec(probe.path().into(), Vec::new(), Vec::new()));
    assert_eq!(env_value(&plain, "TERM").as_deref(), Some("xterm-256color"));
    assert_eq!(env_value(&plain, "CLICOLOR").as_deref(), Some("1"));
    assert_eq!(env_value(&plain, "CLICOLOR_FORCE").as_deref(), Some("1"));
    assert_eq!(env_value(&plain, "COLORTERM").as_deref(), Some("truecolor"));
    assert_eq!(
        env_value(&plain, "FORCE_COLOR"),
        None,
        "FORCE_COLOR is a Node convention, not a general one"
    );

    // An explicit image/Forkfile value wins over our default.
    let explicit = env_of(spec(
        probe.path().into(),
        vec!["TERM=screen-256color".to_owned()],
        vec![PodEnv::new("CLICOLOR_FORCE", "0")],
    ));
    assert_eq!(
        env_value(&explicit, "TERM").as_deref(),
        Some("screen-256color")
    );
    assert_eq!(env_value(&explicit, "CLICOLOR_FORCE").as_deref(), Some("0"));
    assert_eq!(
        env_value(&explicit, "COLORTERM").as_deref(),
        Some("truecolor")
    );

    // A declared `NO_COLOR` opts the pod out of every hint and is never overwritten.
    let off = env_of(spec(
        probe.path().into(),
        Vec::new(),
        vec![PodEnv::new("NO_COLOR", "1")],
    ));
    assert_eq!(env_value(&off, "NO_COLOR").as_deref(), Some("1"));
    assert_eq!(env_value(&off, "TERM"), None);
    assert_eq!(env_value(&off, "CLICOLOR_FORCE"), None);
}

#[test]
fn home_resolution_covers_unset_empty_root_custom_and_root_user() {
    assert!(needs_default_home(1000, None));
    assert!(needs_default_home(1000, Some("")));
    assert!(needs_default_home(1000, Some("/root")));
    assert!(!needs_default_home(1000, Some("/opt/app")));
    assert!(!needs_default_home(0, None));
    assert!(!needs_default_home(0, Some("/root")));
}

#[test]
fn legacy_env_injects_the_writable_home_unless_a_custom_one_is_declared() {
    let probe = TempDir::new_in("/var/tmp").expect("real disk for the rootfs probe");
    let owner = fs::metadata(probe.path()).expect("stat the probe dir");
    let host = host(owner.uid(), owner.gid());
    let env_of = |unset: PodSpec| {
        let pod = PodIdentityMapping::resolve(&unset, &host);
        merged_env(&unset, &pod, host.uid)
    };

    let unset = env_of(spec(probe.path().into(), Vec::new(), Vec::new()));
    assert_eq!(env_value(&unset, "HOME").as_deref(), Some(POD_HOME));
    assert_eq!(
        env_value(&unset, "USER"),
        None,
        "a resource pod invents no login variables"
    );
    assert_eq!(env_value(&unset, "LOGNAME"), None);

    let root = env_of(spec(
        probe.path().into(),
        vec!["HOME=/root".to_owned()],
        Vec::new(),
    ));
    assert_eq!(env_value(&root, "HOME").as_deref(), Some(POD_HOME));

    // A custom image `ENV HOME` still wins for the legacy identity.
    let image = env_of(spec(
        probe.path().into(),
        vec!["HOME=/opt/app".to_owned()],
        Vec::new(),
    ));
    assert_eq!(env_value(&image, "HOME").as_deref(), Some("/opt/app"));

    let empty = env_of(spec(
        probe.path().into(),
        Vec::new(),
        vec![PodEnv::new("HOME", "")],
    ));
    assert_eq!(
        env_value(&empty, "HOME").as_deref(),
        Some(POD_HOME),
        "an empty declared HOME still needs the writable in-pod home"
    );

    let custom = env_of(spec(
        probe.path().into(),
        vec!["HOME=/root".to_owned()],
        vec![PodEnv::new("HOME", "/opt/app")],
    ));
    assert_eq!(env_value(&custom, "HOME").as_deref(), Some("/opt/app"));
}

#[test]
fn an_image_identity_names_its_login_and_an_empty_home_falls_back_to_the_account() {
    let disk = TempDir::new_in("/var/tmp").expect("real disk for the image rootfs");
    let rootfs = seeded_image(
        &disk,
        "root:x:0:0:root:/root:/bin/sh\napp:x:1001:1002:App:/srv/app:/bin/sh\n",
        "root:x:0:\napp:x:1002:\n",
    );
    let owner = fs::metadata(disk.path()).expect("stat the probe dir");
    let pod_host = host(owner.uid(), owner.gid());
    let env_of = |home: Option<&str>| {
        let mut image = spec(rootfs.clone(), Vec::new(), Vec::new());
        image.identity = Some(Identity {
            uid: 1001,
            gid: 1002,
        });
        if let Some(home) = home {
            image.env = vec![PodEnv::new("HOME", home)];
        }
        let pod = PodIdentityMapping::resolve(&image, &pod_host);
        merged_env(&image, &pod, pod_host.uid)
    };

    let account = env_of(None);
    assert_eq!(env_value(&account, "HOME").as_deref(), Some("/srv/app"));
    assert_eq!(env_value(&account, "USER").as_deref(), Some("app"));
    assert_eq!(env_value(&account, "LOGNAME").as_deref(), Some("app"));

    let empty = env_of(Some(""));
    assert_eq!(
        env_value(&empty, "HOME").as_deref(),
        Some("/srv/app"),
        "an empty HOME falls back to the image account's own home"
    );

    let declared = env_of(Some("/opt/app"));
    assert_eq!(env_value(&declared, "HOME").as_deref(), Some("/opt/app"));
}

#[test]
fn an_image_user_uses_its_own_passwd_and_caps_are_root_only() {
    let disk = TempDir::new_in("/var/tmp").expect("real disk for the image rootfs");
    let rootfs = seeded_image(
        &disk,
        "root:x:0:0:root:/root:/bin/sh\napp:x:1001:1002:App:/srv/app:/bin/zsh\n",
        "root:x:0:\napp:x:1002:\n",
    );
    let owner = fs::metadata(disk.path()).expect("stat the probe dir");
    let pod_host = host(owner.uid(), owner.gid());
    let mut app_spec = spec(rootfs.clone(), vec!["PATH=/usr/bin".to_owned()], Vec::new());
    app_spec.identity = Some(Identity {
        uid: 1001,
        gid: 1002,
    });

    let bundle = write_bundle(&app_spec, &pod_host, &disk.path().join("bundle-app"))
        .expect("write the app bundle");
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(&bundle.config).expect("read config"))
            .expect("parse config");
    assert_eq!(json["process"]["user"]["uid"], 1001);
    assert_eq!(json["process"]["user"]["gid"], 1002);
    assert_eq!(
        json["linux"]["uidMappings"][0]["containerID"], 1001,
        "the container uid maps onto the invoking host uid"
    );
    assert_eq!(json["linux"]["uidMappings"][0]["hostID"], pod_host.uid);
    assert_eq!(
        json["process"]["capabilities"]["bounding"]
            .as_array()
            .expect("bounding")
            .len(),
        0,
        "a non-root USER drops Docker's default caps"
    );
    assert!(
        !bundle.dir.join("passwd").exists(),
        "a named image account keeps the image's own /etc/passwd"
    );
    assert!(
        rootfs.join("srv/app").is_dir(),
        "the account home is materialised"
    );

    let env: Vec<String> = json["process"]["env"]
        .as_array()
        .expect("env")
        .iter()
        .map(|value| value.as_str().expect("env string").to_owned())
        .collect();
    assert_eq!(env_value(&env, "HOME").as_deref(), Some("/srv/app"));
    assert_eq!(env_value(&env, "USER").as_deref(), Some("app"));
    assert_eq!(env_value(&env, "LOGNAME").as_deref(), Some("app"));
    assert_eq!(env_value(&env, "PATH").as_deref(), Some("/usr/bin"));

    // Root keeps the image files too and gets the full Docker capability set.
    let mut root_spec = spec(rootfs.clone(), Vec::new(), Vec::new());
    root_spec.identity = Some(Identity::ROOT);
    let root_bundle = write_bundle(&root_spec, &pod_host, &disk.path().join("bundle-root"))
        .expect("write the root bundle");
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(&root_bundle.config).expect("read config"))
            .expect("parse config");
    assert_eq!(json["process"]["user"]["uid"], 0);
    assert_eq!(
        json["process"]["capabilities"]["bounding"]
            .as_array()
            .expect("bounding")
            .len(),
        14,
        "container root gets Docker's default caps inside the userns"
    );
    assert_eq!(json["linux"]["uidMappings"][0]["containerID"], 0);
    assert_eq!(json["linux"]["uidMappings"][0]["hostID"], pod_host.uid);
    assert!(!root_bundle.dir.join("passwd").exists());
    let env: Vec<String> = json["process"]["env"]
        .as_array()
        .expect("env")
        .iter()
        .map(|value| value.as_str().expect("env string").to_owned())
        .collect();
    assert_eq!(env_value(&env, "HOME").as_deref(), Some("/root"));
    assert_eq!(env_value(&env, "USER").as_deref(), Some("root"));
}

#[test]
fn an_image_root_home_is_never_recreated_or_rechmodded() {
    let disk = TempDir::new_in("/var/tmp").expect("real disk for the image rootfs");
    let rootfs = disk.path().join("rootfs");
    fs::create_dir_all(rootfs.join("root")).expect("seed the image /root");
    fs::set_permissions(rootfs.join("root"), fs::Permissions::from_mode(0o700))
        .expect("the image ships a private /root");
    fs::create_dir_all(rootfs.join("etc")).expect("seed the image etc");
    fs::write(rootfs.join("etc/passwd"), "root:x:0:0:root:/root:/bin/sh\n").expect("write passwd");
    let owner = fs::metadata(disk.path()).expect("stat the probe dir");
    let pod_host = host(owner.uid(), owner.gid());

    let mut root_spec = spec(rootfs.clone(), Vec::new(), Vec::new());
    root_spec.identity = Some(Identity::ROOT);
    write_bundle(&root_spec, &pod_host, &disk.path().join("bundle-root"))
        .expect("write the root bundle");
    let meta = fs::metadata(rootfs.join("root")).expect("stat the image /root");
    assert_eq!(
        meta.mode() & 0o777,
        0o700,
        "an image-owned /root keeps its mode"
    );
}

#[test]
fn bundle_materialises_a_0755_home_owned_by_the_pod_user() {
    let disk = TempDir::new_in("/var/tmp").expect("real disk for the pod rootfs");
    let owner = fs::metadata(disk.path()).expect("stat the probe dir");
    let pod_host = host(owner.uid(), owner.gid());
    let rootfs = disk.path().join("rootfs");
    fs::create_dir_all(&rootfs).expect("seed the rootfs");

    let bundle = write_bundle(
        &spec(rootfs.clone(), Vec::new(), Vec::new()),
        &pod_host,
        &disk.path().join("bundle"),
    )
    .expect("write the OCI bundle");

    let home = rootfs.join("home/slipfork");
    let meta = fs::metadata(&home).expect("the baseline home exists on disk");
    assert!(meta.is_dir());
    assert_eq!(meta.mode() & 0o777, 0o755);
    assert_eq!((meta.uid(), meta.gid()), (pod_host.uid, pod_host.gid));
    let rendered_passwd =
        String::from_utf8(fs::read(bundle.dir.join("passwd")).expect("read passwd"))
            .expect("the synthetic passwd is UTF-8");
    assert!(rendered_passwd.contains("/home/slipfork:/bin/sh"));
    let rendered_group = String::from_utf8(fs::read(bundle.dir.join("group")).expect("read group"))
        .expect("the synthetic group is UTF-8");
    assert!(rendered_group.contains("slipfork"));

    // A declared custom `HOME` wins and still gets a real directory on disk.
    let custom_rootfs = disk.path().join("rootfs-custom");
    fs::create_dir_all(&custom_rootfs).expect("seed the second rootfs");
    let custom = write_bundle(
        &spec(
            custom_rootfs.clone(),
            vec!["HOME=/root".to_owned()],
            vec![PodEnv::new("HOME", "/opt/app")],
        ),
        &pod_host,
        &disk.path().join("bundle-custom"),
    )
    .expect("write the custom bundle");
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(&custom.config).expect("read config"))
            .expect("parse config");
    assert!(
        json["process"]["env"]
            .as_array()
            .expect("env")
            .iter()
            .any(|value| value == "HOME=/opt/app")
    );
    assert!(custom_rootfs.join("home/slipfork").is_dir());
}

#[test]
fn root_pods_keep_root_home_and_skip_the_baseline_directory() {
    let disk = TempDir::new_in("/var/tmp").expect("real disk for the root pod rootfs");
    let rootfs = disk.path().join("rootfs");
    fs::create_dir_all(&rootfs).expect("seed the rootfs");
    let bundle = write_bundle(
        &spec(rootfs.clone(), Vec::new(), Vec::new()),
        &host(0, 0),
        &disk.path().join("bundle"),
    )
    .expect("write the root bundle");
    assert!(!rootfs.join("home/slipfork").exists());
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(&bundle.config).expect("read config"))
            .expect("parse config");
    let env: Vec<String> = json["process"]["env"]
        .as_array()
        .expect("env")
        .iter()
        .map(|value| value.as_str().expect("env string").to_owned())
        .collect();
    assert_eq!(
        env_value(&env, "HOME"),
        None,
        "a uid-0 legacy pod keeps the image's own HOME"
    );
    assert_eq!(env_value(&env, "USER"), None);
}
