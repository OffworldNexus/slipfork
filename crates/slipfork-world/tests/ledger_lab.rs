//! Real-disk lab: `state.db` modes/WAL, the caller-owned daemon `flock`, the migration gate,
//! transactional splice, target resolution, flat fork-of-fork lineage, and row lifecycle.
//! No mocks, no Btrfs; every directory is a real `/var/tmp` tempdir.

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, Database, DatabaseConnection, EntityTrait,
    QueryFilter,
};
use sea_orm_migration::seaql_migrations;
use slipfork_world::ledger::entity::worlds;
use slipfork_world::ledger::{
    ChainEntry, ForkLedger, ForkRequest, ForkRow, LiveFork, RootRequest, StatePaths,
};
use slipfork_world::{ForkName, LedgerError, ProjectScope, StoreLayout, WorldName, WorldState};
use tempfile::TempDir;

const NOW: i64 = 1_700_000_000_000;
const STORE: &str = "/var/tmp/slipfork-ledger-store";

fn lab() -> TempDir {
    tempfile::Builder::new()
        .tempdir_in("/var/tmp")
        .expect("real /var/tmp lab dir")
}

fn paths(dir: &Path) -> StatePaths {
    StatePaths::new(dir.join("state"))
}

fn daemon_lock(state: &StatePaths) -> File {
    fs::create_dir_all(&state.state_dir).expect("state dir");
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(state.state_dir.join("daemon.lock"))
        .expect("daemon lock")
}

fn writer(state: &StatePaths) -> ForkLedger {
    ForkLedger::open_writer(state.clone(), daemon_lock(state)).expect("writer")
}

fn world(name: &str) -> WorldName {
    WorldName::parse(name).expect("valid world name")
}

fn root_request(name: &str, dir: &Path) -> RootRequest {
    RootRequest {
        name: world(name),
        checkout_path: dir.join("proj").join(name),
        store_root: PathBuf::from(STORE),
        now_ms: NOW,
    }
}

fn request(root_id: i64, name: &str, parent_id: i64) -> ForkRequest {
    ForkRequest {
        root_id,
        name: ForkName::parse(name).expect("valid fork name"),
        parent_id,
        runtime_dir: PathBuf::from("/run/user/1000"),
        branch: name.to_owned(),
        now_ms: NOW,
    }
}

fn begin(ledger: &ForkLedger, root_id: i64, name: &str, parent_id: i64) -> ForkRow {
    ledger
        .begin_fork(request(root_id, name, parent_id))
        .expect("fork row")
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
}

async fn open_dev(state: &StatePaths) -> DatabaseConnection {
    Database::connect(format!("sqlite://{}", state.db_file().display()))
        .await
        .expect("dev connection")
}

fn row(ledger: &ForkLedger, id: i64) -> slipfork_world::WorldRow {
    ledger
        .list()
        .expect("rows")
        .worlds
        .into_iter()
        .find(|world| world.id == id)
        .expect("world row")
}
#[test]
fn writer_lock_schema_and_absent_reader() {
    let dir = lab();
    let state = paths(dir.path());
    let ledger = ForkLedger::open_writer(state.clone(), daemon_lock(&state)).expect("writer");
    let db = state.db_file();
    assert_eq!(mode(&db), 0o600);
    let root = ledger
        .register_root(root_request("test1", dir.path()))
        .expect("root");
    assert_eq!(root.world_name, "test1");
    for suffix in ["db-wal", "db-shm"] {
        assert!(db.with_extension(suffix).exists(), "{suffix} exists");
    }
    assert_eq!(ledger.applied_versions().expect("versions").len(), 1);
    let reader = ForkLedger::open_reader(state.state_dir.clone()).expect("reader");
    assert_eq!(reader.list().expect("rows").worlds.len(), 1);
    assert!(matches!(
        reader.register_root(root_request("nope", dir.path())),
        Err(LedgerError::ReadOnly)
    ));
    let busy = ForkLedger::open_writer(state.clone(), daemon_lock(&state))
        .expect_err("second writer refused");
    assert!(matches!(busy, LedgerError::WriterBusy), "{busy}");
    drop(ledger);
    let reopened = ForkLedger::open_writer(state.clone(), daemon_lock(&state)).expect("reopen");
    assert_eq!(
        reopened.list().expect("rows").worlds.len(),
        1,
        "rows survive"
    );
    assert_eq!(reopened.applied_versions().expect("versions").len(), 1);
    drop(reopened);
    let empty_dir = lab();
    let empty = ForkLedger::open_reader(empty_dir.path().to_path_buf()).expect("empty reader");
    assert!(empty.list().expect("empty rows").is_empty());
    assert!(
        empty
            .resolve(&ProjectScope::None, "ghost")
            .expect("empty resolve")
            .is_none()
    );
}

#[test]
fn migration_gate_refuses_pending_and_newer() {
    let dir = lab();
    let state = paths(dir.path());
    let ledger = writer(&state);
    ledger
        .register_root(root_request("test1", dir.path()))
        .expect("root");
    drop(ledger);
    let rt = runtime();
    rt.block_on(async {
        let conn = open_dev(&state).await;
        seaql_migrations::Entity::delete_many()
            .exec(&conn)
            .await
            .expect("wipe versions");
        conn.close().await.expect("close");
    });
    let pending = ForkLedger::open_reader(state.state_dir.clone()).expect_err("pending");
    assert!(
        pending.to_string().contains("next: slipfork up"),
        "{pending}"
    );
    let reopened = writer(&state);
    assert_eq!(
        reopened.list().expect("rows").worlds.len(),
        1,
        "row survives"
    );
    assert_eq!(reopened.applied_versions().expect("versions").len(), 1);
    drop(reopened);
    rt.block_on(async {
        let conn = open_dev(&state).await;
        seaql_migrations::Entity::insert(seaql_migrations::ActiveModel {
            version: Set("m9999_planted".to_owned()),
            applied_at: Set(NOW),
        })
        .exec(&conn)
        .await
        .expect("plant newer version");
        conn.close().await.expect("close");
    });
    let newer = ForkLedger::open_writer(state.clone(), daemon_lock(&state))
        .expect_err("newer schema refused");
    assert!(
        newer.to_string().contains("next: upgrade slipfork"),
        "{newer}"
    );
}

#[test]
fn delete_splices_children_and_rejects_unknown_and_roots() {
    let dir = lab();
    let state = paths(dir.path());
    let ledger = writer(&state);
    let root = ledger
        .register_root(root_request("test1", dir.path()))
        .expect("root");
    let bonjour = begin(&ledger, root.id, "bonjour", root.id);
    let salut = begin(&ledger, root.id, "salut", bonjour.world_id);

    assert!(matches!(
        ledger.delete_world_row(9999),
        Err(LedgerError::UnknownRow { id: 9999 })
    ));
    let refused = ledger.delete_world_row(root.id).expect_err("root refused");
    assert!(matches!(refused, LedgerError::RootRow { .. }));
    assert!(
        refused.to_string().contains("next: slipfork down test1"),
        "{refused}"
    );

    ledger
        .delete_world_row(bonjour.world_id)
        .expect("delete fork");
    let rows = ledger.list().expect("rows");
    assert_eq!(rows.worlds.len(), 2);
    assert_eq!(rows.forks.len(), 1, "bonjour row cascaded");
    let kept = rows
        .forks
        .iter()
        .find(|fork| fork.world_id == salut.world_id)
        .expect("salut row");
    assert_eq!(kept.parent_id, root.id, "spliced to the root");
}
#[test]
fn fork_of_fork_lands_flat_in_the_root() {
    let dir = lab();
    let state = paths(dir.path());
    let ledger = writer(&state);
    let root = ledger
        .register_root(root_request("test1", dir.path()))
        .expect("root");
    let bonjour = begin(&ledger, root.id, "bonjour", root.id);
    let salut = begin(&ledger, root.id, "salut", bonjour.world_id);
    assert_eq!(salut.parent_id, bonjour.world_id);
    assert_eq!(salut.root_id, root.id);
    let expected = dir.path().join("proj/test1/.forks/salut");
    let listed = ledger.list().expect("rows");
    let listed = listed
        .worlds
        .iter()
        .find(|world| world.id == salut.world_id)
        .expect("salut row");
    assert_eq!(listed.checkout_path, expected);
    assert_eq!(row(&ledger, salut.world_id).checkout_path, expected);
    let other = ledger
        .register_root(RootRequest {
            checkout_path: dir.path().join("proj/test2"),
            ..root_request("test2", dir.path())
        })
        .expect("other root");
    let foreign = begin(&ledger, other.id, "bonjour", other.id);
    let cases = [
        (root.id, "intruder", foreign.world_id),
        (bonjour.world_id, "nested", bonjour.world_id),
        (bonjour.world_id, "nested2", root.id),
    ];
    for (family, name, parent) in cases {
        let refused = ledger
            .begin_fork(request(family, name, parent))
            .expect_err("foreign parent");
        assert!(
            matches!(refused, LedgerError::ForeignParent { root_id, .. } if root_id == family),
            "{refused}"
        );
    }
}
#[test]
fn unique_conflicts_share_store_name_and_fork_family() {
    let dir = lab();
    let state = paths(dir.path());
    let ledger = writer(&state);
    let root = ledger
        .register_root(root_request("test1", dir.path()))
        .expect("root");
    let duplicate = ledger
        .register_root(RootRequest {
            checkout_path: dir.path().join("elsewhere/test1"),
            ..root_request("test1", dir.path())
        })
        .expect_err("duplicate store/name");
    assert!(
        matches!(duplicate, LedgerError::NameTaken { .. }),
        "{duplicate}"
    );
    assert!(
        duplicate.to_string().contains("next: slipfork down test1"),
        "{duplicate}"
    );
    begin(&ledger, root.id, "bonjour", root.id);
    drop(ledger);

    let rt = runtime();
    rt.block_on(async {
        let conn = open_dev(&state).await;
        conn.execute_unprepared(&format!(
            "INSERT INTO worlds (checkout_path, world_name, store_root, state, updated_at_ms) VALUES ('/var/tmp/other/bonjour', 'bonjour--other', '{STORE}', 'stopped', 0)"
        ))
        .await
        .expect("other world");
        let id = worlds::Entity::find()
            .filter(worlds::Column::WorldName.eq("bonjour--other"))
            .one(&conn)
            .await
            .expect("query")
            .expect("row")
            .id;
        let error = conn
            .execute_unprepared(&format!(
                "INSERT INTO forks (world_id, root_id, parent_id, name, branch, created_at_ms) VALUES ({id}, {}, {}, 'bonjour', '', 0)",
                root.id, root.id
            ))
            .await
            .expect_err("duplicate family name");
        assert!(error.to_string().contains("UNIQUE"), "{error}");
        conn.close().await.expect("close");
    });

    let ledger = writer(&state);
    let inserted = begin(&ledger, root.id, "salut", root.id);
    assert_eq!(inserted.name, "salut");
    let forks = ledger.list().expect("rows").forks;
    assert_eq!(
        forks.iter().filter(|fork| fork.root_id == root.id).count(),
        2
    );
}

#[test]
fn resolve_inside_and_outside_a_family() {
    let dir = lab();
    let state = paths(dir.path());
    let ledger = writer(&state);
    let one = dir.path().join("proj/test1");
    let two = dir.path().join("proj/test2");
    let test1 = ledger
        .register_root(RootRequest {
            checkout_path: one.clone(),
            ..root_request("test1", dir.path())
        })
        .expect("test1");
    let test2 = ledger
        .register_root(RootRequest {
            checkout_path: two.clone(),
            ..root_request("test2", dir.path())
        })
        .expect("test2");
    let bonjour = begin(&ledger, test1.id, "bonjour", test1.id);
    begin(&ledger, test1.id, "salut", bonjour.world_id);
    begin(&ledger, test2.id, "bonjour", test2.id);

    let root_scope = ProjectScope::Root {
        root: one.clone(),
        world: "test1".to_owned(),
        forks: Vec::new(),
        problem: None,
    };
    let inside = ledger
        .resolve(&root_scope, "bonjour")
        .expect("resolve")
        .expect("some");
    assert_eq!(inside.fork.expect("fork").root_id, test1.id);
    let fork_scope = ProjectScope::Fork {
        path: one.join(".forks/bonjour"),
        name: "bonjour".to_owned(),
        world: "bonjour--test1".to_owned(),
    };
    let sibling = ledger
        .resolve(&fork_scope, "salut")
        .expect("resolve")
        .expect("some");
    assert_eq!(sibling.fork.expect("fork").name, "salut");
    let root_again = ledger
        .resolve(&fork_scope, "test1")
        .expect("resolve")
        .expect("some");
    assert!(root_again.fork.is_none(), "family root resolves");
    let outside = ledger
        .resolve(&ProjectScope::None, "salut")
        .expect("unique outside")
        .expect("some");
    assert_eq!(outside.world.world_name, "salut--test1");
    assert!(matches!(
        ledger.resolve(&root_scope, "ghost"),
        Err(LedgerError::UnknownTarget { .. })
    ));

    let ambiguous = ledger
        .resolve(&ProjectScope::None, "bonjour")
        .expect_err("ambiguous");
    let message = ambiguous.to_string();
    assert!(message.contains("bonjour--test1"), "{message}");
    assert!(message.contains("bonjour--test2"), "{message}");
    assert!(message.contains("next: slipfork drop"), "{message}");

    ledger
        .register_root(RootRequest {
            checkout_path: dir.path().join("proj/salut"),
            ..root_request("salut", dir.path())
        })
        .expect("bare root");
    let mixed = ledger
        .resolve(&ProjectScope::None, "salut")
        .expect_err("root plus fork");
    let message = mixed.to_string();
    assert!(message.contains("salut--test1"), "{message}");
    assert!(
        !message.contains("next: slipfork drop salut\n"),
        "{message}"
    );
}

#[test]
fn lifecycle_transitions_and_validation() {
    let dir = lab();
    let state = paths(dir.path());
    let ledger = writer(&state);
    let root = ledger
        .register_root(root_request("test1", dir.path()))
        .expect("root");
    let bonjour = begin(&ledger, root.id, "bonjour", root.id);
    let id = bonjour.world_id;

    ledger
        .publish_live(
            id,
            LiveFork {
                supervisor_pid: 42,
                pods_dir: PathBuf::from("/run/user/1000/slipfork/pods/bonjour--test1"),
                started_at_ms: NOW,
            },
            NOW + 5,
        )
        .expect("publish live");
    let live = row(&ledger, id);
    assert_eq!(live.state, WorldState::Live);
    assert_eq!(live.supervisor_pid, Some(42));
    assert!(live.pods_dir.is_some());
    assert_eq!(live.started_at_ms, Some(NOW));
    assert!(live.problem.is_none());
    assert_eq!(live.updated_at_ms, NOW + 5, "mutation clock");

    ledger.mark_stopping(id, NOW + 6).expect("stopping");
    assert_eq!(row(&ledger, id).state, WorldState::Stopping);
    ledger.mark_stopped(id, NOW + 7).expect("stopped");
    let stopped = row(&ledger, id);
    assert_eq!(stopped.state, WorldState::Stopped);
    assert!(stopped.supervisor_pid.is_none());
    assert!(stopped.pods_dir.is_none());
    assert!(stopped.problem.is_none());
    ledger.mark_deleting(id, NOW + 8).expect("deleting");
    assert_eq!(row(&ledger, id).state, WorldState::Deleting);
    ledger.delete_world_row(id).expect("delete");
    assert!(ledger.list().expect("rows").forks.is_empty());

    let collision = ledger
        .begin_fork(request(root.id, "test1", root.id))
        .expect_err("collision");
    assert!(matches!(collision, LedgerError::NameCollision { .. }));
    let mut deep = request(root.id, "longpath", root.id);
    deep.runtime_dir = PathBuf::from(format!("/run/user/1000/{}", "x".repeat(120)));
    let too_long = ledger.begin_fork(deep).expect_err("socket length");
    assert!(
        matches!(too_long, LedgerError::NameTooLong { kind, .. } if kind == "control socket path"),
        "{too_long}"
    );
    assert!(matches!(
        ledger.mark_stopping(9999, NOW),
        Err(LedgerError::UnknownRow { id: 9999 })
    ));
}

#[test]
fn ensure_chain_registers_root_and_fork_then_stays_a_pure_read() {
    let dir = lab();
    let state = paths(dir.path());
    let ledger = writer(&state);
    let root_path = dir.path().join("proj/test1");
    let fork = ForkName::parse("feat").expect("valid fork name");
    let fork_path = StoreLayout::project_fork(&root_path, &fork);
    let chain = vec![
        ChainEntry {
            world_name: world("test1"),
            checkout_path: root_path,
            fork: None,
        },
        ChainEntry {
            world_name: world("feat--test1"),
            checkout_path: fork_path.clone(),
            fork: Some(fork),
        },
    ];

    let registered = ledger
        .ensure_chain(PathBuf::from(STORE), chain.clone(), NOW)
        .expect("register the chain");
    assert_eq!(registered.world_name, "feat--test1");
    assert_eq!(registered.checkout_path, fork_path);
    assert_eq!(registered.state, WorldState::Stopped);
    let rows = ledger.list().expect("rows");
    assert_eq!(rows.worlds.len(), 2, "{rows:?}");
    let root = rows
        .worlds
        .iter()
        .find(|row| row.world_name == "test1")
        .expect("the root is inserted");
    let edge = rows
        .forks
        .iter()
        .find(|edge| edge.world_id == registered.id)
        .expect("the fork edge is inserted");
    assert_eq!(edge.root_id, root.id);
    assert_eq!(edge.parent_id, root.id);
    assert_eq!(edge.name, "feat");
    assert_eq!(edge.branch, "", "a chain walk never invents a branch");

    let again = ledger
        .ensure_chain(PathBuf::from(STORE), chain, NOW + 1)
        .expect("an already-current chain");
    assert_eq!(again, registered, "the innermost row is unchanged");
    let reread = ledger.list().expect("rows");
    assert_eq!(
        reread.worlds, rows.worlds,
        "an already-current chain writes no worlds row"
    );
    assert_eq!(
        reread.forks, rows.forks,
        "an already-current chain writes no forks row"
    );
}

#[test]
fn ensure_chain_repairs_a_root_ish_fork_row_from_disk() {
    let dir = lab();
    let state = paths(dir.path());
    let fork = ForkName::parse("feat").expect("valid fork name");
    let root_path = dir.path().join("proj/test1");
    let fork_path = StoreLayout::project_fork(&root_path, &fork);
    drop(writer(&state));
    // The bug: an `up` in the fork registered the fork checkout as a root, without its ancestor.
    // `register_root` now refuses a `.forks` checkout, so the historical row is seeded raw.
    let rt = runtime();
    let orphan_id = rt.block_on(async {
        let conn = open_dev(&state).await;
        conn.execute_unprepared(&format!(
            "INSERT INTO worlds (checkout_path, world_name, store_root, state, updated_at_ms) VALUES ('{}', 'feat--test1', '{STORE}', 'stopped', {NOW})",
            fork_path.display()
        ))
        .await
        .expect("root-ish fork row");
        let id = worlds::Entity::find()
            .filter(worlds::Column::WorldName.eq("feat--test1"))
            .one(&conn)
            .await
            .expect("query")
            .expect("row")
            .id;
        conn.close().await.expect("close");
        id
    });
    let ledger = writer(&state);
    let chain = vec![
        ChainEntry {
            world_name: world("test1"),
            checkout_path: root_path,
            fork: None,
        },
        ChainEntry {
            world_name: world("feat--test1"),
            checkout_path: fork_path,
            fork: Some(fork),
        },
    ];

    let repaired = ledger
        .ensure_chain(PathBuf::from(STORE), chain, NOW + 1)
        .expect("repair the chain");
    assert_eq!(repaired.id, orphan_id, "the on-disk checkout keeps its row");
    let rows = ledger.list().expect("rows");
    let root = rows
        .worlds
        .iter()
        .find(|row| row.world_name == "test1")
        .expect("the missing root is inserted");
    assert_eq!(root.state, WorldState::Stopped);
    let edge = rows
        .forks
        .iter()
        .find(|edge| edge.world_id == orphan_id)
        .expect("the missing edge is inserted");
    assert_eq!(edge.root_id, root.id);
    assert_eq!(edge.parent_id, root.id);
    assert_eq!(edge.name, "feat");
}

#[test]
fn ensure_chain_never_reparents_a_live_fork_edge() {
    let dir = lab();
    let state = paths(dir.path());
    let ledger = writer(&state);
    let root_path = dir.path().join("proj/test1");
    let root = ledger
        .register_root(RootRequest {
            checkout_path: root_path.clone(),
            ..root_request("test1", dir.path())
        })
        .expect("root");
    let feat = begin(&ledger, root.id, "feat", root.id);
    let jello = begin(&ledger, root.id, "jello", feat.world_id);
    assert_eq!(
        jello.parent_id, feat.world_id,
        "the fork keeps its real parent"
    );

    // Flat layouts can only derive [root, jello] from the checkout path; that topology-blind
    // walk must never flatten the live `jello -> feat` edge back onto the root.
    let jello_name = ForkName::parse("jello").expect("valid fork name");
    let jello_path = StoreLayout::project_fork(&root_path, &jello_name);
    let chain = vec![
        ChainEntry {
            world_name: world("test1"),
            checkout_path: root_path,
            fork: None,
        },
        ChainEntry {
            world_name: world("jello--test1"),
            checkout_path: jello_path,
            fork: Some(jello_name),
        },
    ];
    let registered = ledger
        .ensure_chain(PathBuf::from(STORE), chain, NOW + 1)
        .expect("register the child checkout");
    assert_eq!(registered.id, jello.world_id);

    let rows = ledger.list().expect("rows");
    let edge = rows
        .forks
        .iter()
        .find(|edge| edge.world_id == jello.world_id)
        .expect("the live edge");
    assert_eq!(
        edge.parent_id, feat.world_id,
        "a live edge is never re-parented from path topology"
    );
    assert_eq!(edge.root_id, root.id);
    assert_eq!(edge.name, "jello");
}

#[test]
fn ensure_chain_refuses_a_missing_world_whose_name_is_taken() {
    let dir = lab();
    let state = paths(dir.path());
    let ledger = writer(&state);
    ledger
        .register_root(root_request("test1", dir.path()))
        .expect("root");
    let chain = vec![ChainEntry {
        world_name: world("test1"),
        checkout_path: dir.path().join("elsewhere/test1"),
        fork: None,
    }];
    let refused = ledger
        .ensure_chain(PathBuf::from(STORE), chain, NOW)
        .expect_err("a taken world name must fail typed");
    assert!(
        matches!(refused, LedgerError::NameTaken { ref world_name } if world_name == "test1"),
        "{refused}"
    );
    assert!(
        refused.to_string().contains("next: slipfork down test1"),
        "{refused}"
    );
}

#[test]
fn runs_and_drops_inside_a_tokio_context() {
    let dir = lab();
    let state = paths(dir.path());
    let rt = runtime();
    rt.block_on(async {
        let ledger = ForkLedger::open_writer(state.clone(), daemon_lock(&state)).expect("writer");
        assert!(ledger.list().expect("list").worlds.is_empty());
        ledger
            .register_root(root_request("test1", dir.path()))
            .expect("root");
        assert_eq!(ledger.list().expect("list").worlds.len(), 1);
        drop(ledger);
        let absent = lab();
        let reader = ForkLedger::open_reader(absent.path().to_path_buf()).expect("absent reader");
        assert!(reader.state_db().is_absent());
        assert!(reader.list().expect("list").is_empty());
        drop(reader);
    });
}
