//! Queue rows are durable independently of the native index and only the held leader may claim.
use baleyg::{indexer::IndexOptions, store::Store};
use std::fs;

#[test]
fn durable_fifo_and_incarnation_fence() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("a.js"), "function a() {}\n").unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let options = IndexOptions::new(workspace.path().to_owned());
    let a = store.enqueue_request(&options, None).unwrap();
    let b = store.enqueue_request(&options, None).unwrap();
    assert_eq!(
        (
            a.state.as_str(),
            a.started_at.as_deref(),
            a.finished_at.as_deref()
        ),
        ("queued", None, None)
    );
    assert!(b.seq > a.seq);
    assert_eq!(store.current_request().unwrap().unwrap().id, b.id);
    assert!(store.request_by_id(&a.id).unwrap().is_some());
    let owner = store.leader_session().unwrap();
    let first = store.claim_request(&owner).unwrap().unwrap();
    assert_eq!(first.id, a.id);
    assert_eq!(first.state, "running");
    assert!(first.started_at.is_some());
    assert!(
        store.claim_request(&owner).unwrap().is_none(),
        "running head cannot be skipped"
    );
    store
        .finish_request(&owner, &first, Err(anyhow::anyhow!("index failed")))
        .unwrap();
    let second = store.claim_request(&owner).unwrap().unwrap();
    assert_eq!(second.id, b.id);
    store
        .finish_request(&owner, &second, Err(anyhow::anyhow!("index failed")))
        .unwrap();
    drop(owner);
    drop(store);
    let reopened = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let saved = reopened.request_by_id(&a.id).unwrap().unwrap();
    assert_eq!(saved.state, "failed");
    assert!(saved.finished_at.is_some());
    assert_eq!(reopened.current_request().unwrap().unwrap().id, b.id);
    let more = reopened.enqueue_request(&options, None).unwrap();
    assert!(more.seq > b.seq);
}

#[test]
fn request_rejects_relative_external_input_before_ack() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let mut options = IndexOptions::new(workspace.path().to_owned());
    options.scip_path = Some("relative.scip".into());
    assert!(store.enqueue_request(&options, None).is_err());
    assert!(store.current_request().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn request_rejects_symlink_to_captured_workspace() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let link = state.path().join("root-link");
    std::os::unix::fs::symlink(workspace.path(), &link).unwrap();
    let options = IndexOptions::new(link);
    assert!(store.enqueue_request(&options, None).is_err());
    assert!(store.current_request().unwrap().is_none());
}

#[test]
fn first_cli_takeover_capture_satisfies_fifo_head_once() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("a.js"), "function seed() {}\n").unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let options = IndexOptions::new(workspace.path().to_owned());
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (pin, session) =
        baleyg::index_coordinator::enqueue_and_wait(&store, &options, &cancel).unwrap();
    assert_eq!(pin.index_revision, 1);
    assert!(session.is_leader());
    assert_eq!(store.status().unwrap().revision, pin);
    let head = store.current_request().unwrap().unwrap();
    assert_eq!(head.state, "done");
    assert_eq!(head.revision, Some(pin));
}

#[test]
fn concurrent_first_queue_open_is_atomic_and_fifo() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let options = IndexOptions::new(workspace.path().to_owned());
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers = (0..2)
        .map(|_| {
            let store = store.clone();
            let options = options.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                store.enqueue_request(&options, None).unwrap()
            })
        })
        .collect::<Vec<_>>();
    let mut accepted = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    accepted.sort_by_key(|request| request.seq);
    assert_eq!((accepted[0].seq, accepted[1].seq), (1, 2));
    assert_eq!(store.current_request().unwrap().unwrap().id, accepted[1].id);
    assert_eq!(
        store.earliest_unfinished_request().unwrap().unwrap().id,
        accepted[0].id
    );
}

#[test]
fn incompatible_or_corrupt_existing_queue_never_gets_fresh_ack() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let options = IndexOptions::new(workspace.path().to_owned());
    let accepted = store.enqueue_request(&options, None).unwrap();
    let path = store.request_db_path();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.pragma_update(None, "user_version", 2).unwrap();
    drop(db);
    assert!(
        store
            .enqueue_request(&options, None)
            .unwrap_err()
            .to_string()
            .contains("incompatible_queue")
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    db.pragma_update(None, "user_version", 1).unwrap();
    db.execute(
        "UPDATE queue_identity SET root_key='wrong' WHERE singleton=1",
        [],
    )
    .unwrap();
    drop(db);
    assert!(
        store
            .enqueue_request(&options, None)
            .unwrap_err()
            .to_string()
            .contains("root_key_collision")
    );
    fs::write(&path, b"not a sqlite queue").unwrap();
    assert!(store.enqueue_request(&options, None).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"not a sqlite queue");
    assert!(!accepted.id.is_empty());
}

#[test]
fn queue_hot_journal_child() {
    let Some(path) = std::env::var_os("BALEYG_QUEUE_HOT_JOURNAL_CHILD") else {
        return;
    };
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA cache_size=1; BEGIN IMMEDIATE; UPDATE queue_identity SET root_key='bad'; UPDATE requests SET options_json=zeroblob(65536)").unwrap();
    // Abrupt process exit intentionally leaves SQLite's actual uncommitted rollback journal.
    unsafe { libc::_exit(17) }
}

#[test]
fn hot_journal_rolls_back_before_queue_identity_and_ack() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let options = IndexOptions::new(workspace.path().to_owned());
    let accepted = store.enqueue_request(&options, None).unwrap();
    let db_path = store.request_db_path();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("queue_hot_journal_child")
        .env("BALEYG_QUEUE_HOT_JOURNAL_CHILD", &db_path)
        .output()
        .unwrap();
    assert_eq!(
        child.status.code(),
        Some(17),
        "journal helper did not exit as planned"
    );
    let journal = db_path.with_file_name(format!(
        "{}-journal",
        db_path.file_name().unwrap().to_string_lossy()
    ));
    assert!(journal.exists(), "helper must leave an actual hot journal");
    let next = store.enqueue_request(&options, None).unwrap();
    assert!(next.seq > accepted.seq);
    let original = store.request_by_id(&accepted.id).unwrap().unwrap();
    assert_eq!(original.options_json, accepted.options_json);
    assert_eq!(original.state, "queued");
}
