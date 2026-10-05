use baleyg::{indexer::IndexOptions, store::Store};
use rusqlite::Connection;
use std::{path::Path, process::Command, time::Duration};

// A separate process detects lost POSIX fcntl locks; a second thread cannot.
#[test]
fn sqlite_writer_child() {
    let Some(path) = std::env::var_os("BALEYG_WITNESS_LOCK_CHILD") else {
        return;
    };
    let db = Connection::open(path).unwrap();
    db.busy_timeout(Duration::ZERO).unwrap();
    let update = if std::env::var_os("BALEYG_WITNESS_QUEUE").is_some() {
        "UPDATE queue_identity SET root_key=root_key"
    } else {
        "UPDATE index_metadata SET last_opened_at=last_opened_at"
    };
    match db.execute_batch(&format!("BEGIN IMMEDIATE; {update}; COMMIT")) {
        Err(rusqlite::Error::SqliteFailure(info, _))
            if matches!(
                info.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) => {}
        Ok(()) => std::process::exit(17),
        Err(_) => std::process::exit(19),
    }
}

fn contender(path: &Path) -> i32 {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.arg("--exact")
        .arg("sqlite_writer_child")
        .env("BALEYG_WITNESS_LOCK_CHILD", path);
    if path.file_name().is_some_and(|name| name == "requests.db") {
        cmd.env("BALEYG_WITNESS_QUEUE", "1");
    }
    cmd.output().unwrap().status.code().unwrap()
}

#[test]
fn live_index_writer_survives_transient_verification_and_status() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let index = std::fs::read_dir(state.path().join("cache/indexes"))
        .unwrap()
        .filter_map(|e| {
            let path = e.unwrap().path().join("index.db");
            path.exists().then_some(path)
        })
        .next()
        .unwrap();
    let writer = Connection::open(&index).unwrap();
    writer
        .execute_batch("BEGIN IMMEDIATE; UPDATE index_metadata SET last_opened_at=last_opened_at")
        .unwrap();
    let _ = store.status();
    let code = contender(&index);
    let commit = writer.execute_batch("COMMIT");
    assert_eq!(
        code, 0,
        "another process stole live SQLite writer lock: {code}; commit: {commit:?}"
    );
    commit.unwrap();
}

#[test]
fn live_request_writer_survives_queue_worker_reads() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let accepted = store
        .enqueue_request(&IndexOptions::new(workspace.path().to_owned()), None)
        .unwrap();
    let queue = store.request_db_path();
    let writer = Connection::open(&queue).unwrap();
    writer
        .execute_batch("BEGIN IMMEDIATE; UPDATE requests SET submitted_at=submitted_at")
        .unwrap();
    let selected = store.clone();
    std::thread::spawn(move || selected.request_by_id(&accepted.id).unwrap())
        .join()
        .unwrap();
    let code = contender(&queue);
    let commit = writer.execute_batch("COMMIT");
    assert_eq!(
        code, 0,
        "another process stole queue writer lock: {code}; commit: {commit:?}"
    );
    commit.unwrap();
}

// Run exceptional recreation in its own process so retained descriptors do not
// mask leaked old inodes behind other tests' process-wide cache.
#[test]
fn corrupt_index_recreation_child() {
    if std::env::var_os("BALEYG_RECREATE_CHILD").is_none() {
        return;
    }
    use baleyg::index_coordinator::reconcile_workspace;
    use std::{
        os::unix::fs::MetadataExt,
        sync::{Arc, atomic::AtomicBool},
    };
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        workspace.path().join("a.js"),
        "function a() {}
",
    )
    .unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let queue = store
        .enqueue_request(&IndexOptions::new(workspace.path().to_owned()), None)
        .unwrap();
    let requests = store.request_db_path();
    let queue_inode = std::fs::metadata(&requests).unwrap().ino();
    let index = std::fs::read_dir(state.path().join("cache/indexes"))
        .unwrap()
        .filter_map(|e| {
            let path = e.unwrap().path().join("index.db");
            path.exists().then_some(path)
        })
        .next()
        .unwrap();
    let old_inode = std::fs::metadata(&index).unwrap().ino();
    drop(store);
    std::fs::write(&index, b"short").unwrap();
    let recovering = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let (pin, _leader) = reconcile_workspace(
        &recovering,
        &IndexOptions::new(workspace.path().to_owned()),
        &Arc::new(AtomicBool::new(false)),
        |_| {},
    )
    .unwrap();
    assert_eq!(pin.index_revision, 1);
    assert_ne!(std::fs::metadata(&index).unwrap().ino(), old_inode);
    assert_eq!(std::fs::metadata(&requests).unwrap().ino(), queue_inode);
    assert_eq!(
        recovering.request_by_id(&queue.id).unwrap().unwrap().id,
        queue.id
    );
}

#[test]
fn corrupt_index_recreation_preserves_queue_and_replaces_only_index_inode() {
    let outcome = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("corrupt_index_recreation_child")
        .env("BALEYG_RECREATE_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        outcome.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&outcome.stdout),
        String::from_utf8_lossy(&outcome.stderr)
    );
}
