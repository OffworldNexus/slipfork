//! The data-home fallback store is created before it is validated, so `up` never sees `ENOENT`.
//!
//! The scratch root lives on `/var/tmp` (disk, never RAM `/tmp`) and no engine is faked: the real
//! `BtrfsEngine` runs its real `create_dir_all` + superblock probe. On this XFS host the resolved
//! store is created and then refused as *not Btrfs*; a Btrfs host completes the same journey with
//! `Ok(())`. Both outcomes must leave the directory in place.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use slipfork_world::{BtrfsEngine, Error, LogStore, StoreLayout, WorldEngine};

/// A unique disk-backed scratch root under `/var/tmp`.
fn scratch() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock reads after the epoch")
        .as_nanos();
    PathBuf::from("/var/tmp").join(format!("slipfork-store-{}-{nanos}", std::process::id()))
}

/// A `/`-mounted project resolves the data-home fallback, and `ensure_store` creates it first.
#[test]
fn the_data_home_store_is_created_before_it_is_validated() {
    let scratch = scratch();
    let data_home = scratch.join("data-home");
    fs::create_dir_all(&data_home).expect("create the data home");
    let project = PathBuf::from("/srv/demo");
    let store_root = LogStore::root_for(&[PathBuf::from("/")], &project, &data_home, true);
    assert_eq!(store_root, data_home.join("slipfork").join("store"));
    assert!(
        !store_root.exists(),
        "the fallback store must start absent for this test to mean anything"
    );

    let engine = BtrfsEngine::new(StoreLayout::at(&store_root));
    let outcome = engine.ensure_store();

    assert!(
        store_root.is_dir(),
        "the resolved store root is created before it is validated"
    );
    match outcome {
        Ok(()) => {
            // A Btrfs host completes the whole store; the created root and its children remain.
            assert!(store_root.join("images").is_dir());
            assert!(store_root.join("worlds").is_dir());
        }
        Err(error) => {
            let message = error.to_string();
            assert!(
                !message.contains("No such file") && !message.contains("os error 2"),
                "the store was never opened before it existed: {message}"
            );
            assert!(
                message.contains(&store_root.display().to_string()),
                "the refusal pins the resolved store root: {message}"
            );
            match error {
                Error::NotBtrfs { path } => assert_eq!(path, store_root),
                other => {
                    panic!("a non-Btrfs scratch root must be refused as not Btrfs, got: {other}")
                }
            }
        }
    }

    fs::remove_dir_all(&scratch).expect("remove the scratch store");
}
