//! Content-key sensitivity: only the bytes and scope an instruction really names may move it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use slipfork_world::{BtrfsEngine, StoreLayout, WorldEngine};

use crate::builder::BuildRequest;
use crate::context::BuildContext;
use crate::dockerfile::{BuildArgs, DockerfileParser};
use crate::layout::ImageLayout;
use crate::puller::OciPuller;
use crate::{Architecture, RegistryCredentials};

use super::planner::PlannedBuild;
use super::{BuildPlanner, RefResolver};

fn lab(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let root = PathBuf::from("/var/tmp").join(format!(
        "slipfork-plan-{prefix}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("the plan lab needs real disk");
    root
}

fn plan(root: &Path, source: &str, args: BuildArgs) -> PlannedBuild {
    let dockerfile = root.join("Dockerfile");
    fs::write(&dockerfile, source).expect("write the Dockerfile");
    let context = BuildContext::load(root).expect("load the real context");
    let stages = DockerfileParser::parse(source).expect("parse the Dockerfile");
    let request = BuildRequest {
        context: root.to_path_buf(),
        dockerfile,
        args,
        target: None,
        externals: BTreeMap::new(),
    };
    let layout = ImageLayout::new(root.join("images"));
    let engine: Arc<dyn WorldEngine> =
        Arc::new(BtrfsEngine::new(StoreLayout::at(root.join("store"))));
    let puller = OciPuller::new(
        engine,
        layout,
        RegistryCredentials::default(),
        Architecture::X86_64,
        "slipfork-tests/0.1".to_owned(),
    );
    let resolver = RefResolver::new(&puller, root.join("images"), false);
    BuildPlanner::new(&request, &stages, &context, &resolver, Architecture::X86_64)
        .plan()
        .expect("plan the build")
}

fn row_key(build: &PlannedBuild, stage: usize, row: usize) -> String {
    build.stages[stage].rows[row]
        .key
        .expect("the row carries a node key")
        .as_hex()
}

#[test]
fn a_copy_source_edit_moves_its_step_and_every_suffix_parent() {
    let root = lab("copy");
    fs::write(root.join("a.txt"), "one").expect("write a");
    fs::write(root.join("b.txt"), "two").expect("write b");
    let source = "FROM scratch\nCOPY a.txt /a.txt\nCOPY b.txt /b.txt\n";
    let first = plan(&root, source, BuildArgs::new());
    let repeat = plan(&root, source, BuildArgs::new());
    assert_eq!(row_key(&first, 0, 1), row_key(&repeat, 0, 1));
    let a_first = row_key(&first, 0, 1);
    let b_first = row_key(&first, 0, 2);
    fs::write(root.join("a.txt"), "edited").expect("edit a");
    let edited = plan(&root, source, BuildArgs::new());
    assert_ne!(row_key(&edited, 0, 1), a_first, "the COPY source key moves");
    assert_ne!(
        row_key(&edited, 0, 2),
        b_first,
        "the suffix step keys off its parent and moves with it"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_run_commands_own_canon_moves_its_key() {
    let root = lab("run");
    let one = plan(&root, "FROM scratch\nRUN echo one\n", BuildArgs::new());
    let two = plan(&root, "FROM scratch\nRUN echo two\n", BuildArgs::new());
    assert_ne!(
        row_key(&one, 0, 1),
        row_key(&two, 0, 1),
        "a `RUN` line is part of its own content key"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_copy_from_source_edit_moves_the_downstream_step() {
    let root = lab("from");
    fs::write(root.join("a.txt"), "one").expect("write a");
    let source =
        "FROM scratch AS build\nCOPY a.txt /a.txt\nFROM scratch\nCOPY --from=build /a.txt /b.txt\n";
    let first = plan(&root, source, BuildArgs::new());
    let first_copy = row_key(&first, 1, 1);
    fs::write(root.join("a.txt"), "edited").expect("edit a");
    let edited = plan(&root, source, BuildArgs::new());
    assert_ne!(
        row_key(&edited, 1, 1),
        first_copy,
        "`COPY --from` keys off the source stage's materialized key"
    );
    assert_ne!(edited.stages[1].head, first.stages[1].head);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn from_stage_inherits_the_prior_config_into_the_child_key() {
    let root = lab("inherit");
    let one = plan(
        &root,
        "FROM scratch AS base\nENV MODE=one\nFROM base\nRUN true\n",
        BuildArgs::new(),
    );
    let two = plan(
        &root,
        "FROM scratch AS base\nENV MODE=two\nFROM base\nRUN true\n",
        BuildArgs::new(),
    );
    assert_ne!(
        row_key(&one, 1, 1),
        row_key(&two, 1, 1),
        "`FROM <stage>` inherits config, so a changed ENV moves the child step"
    );
    assert_ne!(one.stages[1].head, two.stages[1].head);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn arg_scope_is_the_declared_prefix_of_one_stage() {
    let root = lab("args");
    fs::write(root.join("one"), "one").expect("write one");
    fs::write(root.join("two"), "two").expect("write two");
    let source = "FROM scratch\nARG USED=one\nCOPY $USED /app/$USED\n";

    let mut unused_a = BuildArgs::new();
    unused_a.declare("UNUSED", "a");
    let mut unused_b = BuildArgs::new();
    unused_b.declare("UNUSED", "b");
    assert_eq!(
        row_key(&plan(&root, source, unused_a), 0, 2),
        row_key(&plan(&root, source, unused_b), 0, 2),
        "an ARG the Dockerfile never declares may not move a step key"
    );

    let mut used_one = BuildArgs::new();
    used_one.declare("USED", "one");
    let mut used_two = BuildArgs::new();
    used_two.declare("USED", "two");
    assert_ne!(
        row_key(&plan(&root, source, used_one), 0, 2),
        row_key(&plan(&root, source, used_two), 0, 2),
        "a declared and consumed ARG rebuilds its step"
    );

    let late = "FROM scratch\nCOPY one /app/one\nARG LATE=one\nRUN true\n";
    let mut late_a = BuildArgs::new();
    late_a.declare("LATE", "a");
    let mut late_b = BuildArgs::new();
    late_b.declare("LATE", "b");
    let late_a = plan(&root, late, late_a);
    let late_b = plan(&root, late, late_b);
    assert_eq!(
        row_key(&late_a, 0, 1),
        row_key(&late_b, 0, 1),
        "an ARG declared after the COPY is out of scope for it"
    );
    assert_ne!(
        row_key(&late_a, 0, 3),
        row_key(&late_b, 0, 3),
        "the later RUN is in scope of the late ARG"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn metadata_rows_show_resolved_arg_and_env_values() {
    let root = lab("resolved");
    let source =
        "FROM scratch\nARG port=8080\nENV PORT=$port\nENV BASE=/srv\nENV URL=$BASE:$PORT\n";
    let build = plan(&root, source, BuildArgs::new());
    let rows = &build.stages[0].rows;
    let args: Vec<&str> = rows.iter().map(|row| row.args.as_str()).collect();
    assert_eq!(
        args,
        [
            "scratch",
            "port=8080",
            "PORT=8080",
            "BASE=/srv",
            "URL=/srv:8080"
        ],
        "ARG/ENV rows show the values the build resolved, not `$` placeholders"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn metadata_rows_keep_their_place_without_a_node_key() {
    let root = lab("metadata");
    let source =
        "FROM scratch\nARG X\nENV A=B\nUSER n\nWORKDIR /w\nEXPOSE 80\nCMD [\"/bin/true\"]\n";
    let mut stages = DockerfileParser::parse(source).expect("the fixture parses");
    let instructions = stages.remove(0).instructions.len();
    let build = plan(&root, source, BuildArgs::new());
    let rows = &build.stages[0].rows;
    assert_eq!(
        rows.len(),
        1 + instructions,
        "FROM row plus every instruction"
    );
    let keywords: Vec<&str> = rows.iter().map(|row| row.keyword).collect();
    assert_eq!(
        keywords,
        ["FROM", "ARG", "ENV", "USER", "WORKDIR", "EXPOSE", "CMD"]
    );
    for (offset, row) in rows.iter().enumerate() {
        assert_eq!(row.place.index, offset + 1, "rows keep the walked indices");
        assert_eq!(row.place.total, rows.len());
        let keyed = matches!(row.keyword, "FROM" | "WORKDIR");
        assert_eq!(row.key.is_some(), keyed, "{} key", row.keyword);
    }
    let _ = fs::remove_dir_all(&root);
}
