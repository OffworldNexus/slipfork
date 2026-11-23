//! `flush_store` is one `syncfs(2)` on a single descriptor: idempotent and cheap.
//!
//! The scratch store lives on `/var/tmp` (disk, never RAM `/tmp`); `syncfs` works on any
//! filesystem, so this needs no Btrfs mount and fakes no engine.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slipfork_world::{BtrfsEngine, StoreLayout};

fn scratch() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock reads after the epoch")
        .as_nanos();
    let root =
        PathBuf::from("/var/tmp").join(format!("slipfork-flush-{}-{nanos}", std::process::id()));
    fs::create_dir_all(root.join(".slipfork-store/worlds/app_db"))
        .expect("create the scratch store");
    root
}

#[test]
fn flush_store_is_idempotent_and_cheap() {
    let root = scratch();
    let store = root.join(".slipfork-store");
    let pgdata = store.join("worlds/app_db/base");
    fs::write(&pgdata, b"committed rows").expect("write store data");
    let engine = BtrfsEngine::new(StoreLayout::at(&store));

    let first = Instant::now();
    engine.flush_store().expect("first flush");
    let first_elapsed = first.elapsed();

    let second = Instant::now();
    engine
        .flush_store()
        .expect("a second flush is a no-op syscall");
    let second_elapsed = second.elapsed();

    assert_eq!(
        fs::read(&pgdata).expect("read flushed data"),
        b"committed rows"
    );
    // Generous CI ceiling, not a performance budget: a clean flush is microseconds.
    assert!(
        second_elapsed < Duration::from_secs(5),
        "second flush took {second_elapsed:?}"
    );
    eprintln!("syncfs #1 {first_elapsed:?}, #2 {second_elapsed:?}");

    fs::remove_dir_all(&root).expect("remove the scratch store");
}
