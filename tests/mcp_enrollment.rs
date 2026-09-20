//! Slice 1: the enrolled read-only connection and its identity guard.
//!
//! Covers acceptance test 5 (identity) and the read-only WAL qualification in test 7.
use baleyg::{
    mcp::{ErrorCode, enrollment::Enrollment},
    model::*,
    store::Store,
};
use std::{
    fs,
    sync::{Arc, atomic::AtomicBool},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

fn private_state() -> TempDir {
    let state = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    state
}

fn fixture() -> (TempDir, TempDir, Store) {
    let state = private_state();
    let work = tempfile::tempdir().unwrap();
    let store = Store::open(state.path(), work.path()).unwrap();
    (state, work, store)
}

fn node(id: &str) -> Symbol {
    Symbol {
        id: id.into(),
        name: id.into(),
        kind: SymbolKind::Function,
        path: "a.js".into(),
        range: SourceRange {
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 1,
            ..SourceRange::default()
        },
        parent: None,
        accessor: false,
        provenance: Provenance {
            source: "syntax".into(),
            semantic: SemanticState::Unavailable,
        },
    }
}

fn graph() -> Graph {
    Graph {
        files: vec![SourceFile {
            path: "a.js".into(),
            hash: "hash".into(),
            language: "javascript".into(),
            text: "function a() {}".into(),
        }],
        nodes: vec![node("a")],
        ..Graph::default()
    }
}

fn publish(store: &Store, expected: Option<u64>) -> u64 {
    store
        .publish(
            &graph(),
            expected,
            &(Arc::new(AtomicBool::new(false)) as CancelFlag),
        )
        .unwrap()
}

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

/// Replace a database with a different inode, the way an external restore would.
fn replace_with_new_inode(state: &std::path::Path, name: &str) {
    let staging = state.join("staging.db");
    fs::write(&staging, fs::read(state.join(name)).unwrap()).unwrap();
    fs::rename(&staging, state.join(name)).unwrap();
}

#[test]
fn enrolls_against_an_unindexed_store_and_reports_revision_zero() {
    let (state, _work, store) = fixture();
    assert_eq!(store.status().unwrap().revision, 0);
    let e = Enrollment::enroll(state.path()).unwrap();
    assert!(e.is_available());
    assert_eq!(e.current_revision(soon()).unwrap(), 0);
}

#[test]
fn reads_the_published_revision_in_one_transaction() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path()).unwrap();
    let (revision, names) = e
        .read(None, soon(), |c| {
            let mut stmt = c.prepare("SELECT id FROM nodes ORDER BY id")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    assert_eq!(revision, 1);
    assert_eq!(names, vec!["a".to_string()]);
}

#[test]
fn expected_revision_mismatch_is_a_conflict() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path()).unwrap();
    let err = e.read(Some(99), soon(), |_| Ok(())).unwrap_err();
    assert_eq!(err.code, ErrorCode::RevisionConflict);
    // A conflict is not a storage failure: the binding stays usable.
    assert!(e.is_available());
    assert!(e.read(Some(1), soon(), |_| Ok(())).is_ok());
}

#[test]
fn publication_advances_revision_without_rotating_the_generation() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path()).unwrap();
    let generation = e.store_generation();
    assert_eq!(e.current_revision(soon()).unwrap(), 1);
    publish(&store, Some(1));
    assert_eq!(e.current_revision(soon()).unwrap(), 2);
    assert_eq!(e.store_generation(), generation);
    assert!(e.is_available());
}

#[test]
fn replacing_the_cache_latches_the_binding() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path()).unwrap();
    let generation = e.store_generation();
    assert!(e.read(None, soon(), |_| Ok(())).is_ok());

    replace_with_new_inode(state.path(), "cache.db");

    let err = e.read(None, soon(), |_| Ok(())).unwrap_err();
    assert_eq!(err.code, ErrorCode::StoreUnavailable);
    assert!(!e.is_available());
    assert_ne!(e.store_generation(), generation);
}

#[test]
fn replacing_the_durable_database_also_latches_the_binding() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path()).unwrap();
    replace_with_new_inode(state.path(), "workspace.db");
    assert_eq!(
        e.read(None, soon(), |_| Ok(())).unwrap_err().code,
        ErrorCode::StoreUnavailable
    );
    assert!(!e.is_available());
}

#[test]
fn deleting_the_cache_latches_the_binding() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path()).unwrap();
    fs::remove_file(state.path().join("cache.db")).unwrap();
    assert_eq!(
        e.read(None, soon(), |_| Ok(())).unwrap_err().code,
        ErrorCode::StoreUnavailable
    );
    assert!(!e.is_available());
}

#[test]
fn republishing_does_not_clear_the_latch() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path()).unwrap();
    e.invalidate();
    let rotated = e.store_generation();

    // Browser or owner activity that republishes the index must not restore MCP availability.
    publish(&store, Some(1));

    assert_eq!(
        e.read(None, soon(), |_| Ok(())).unwrap_err().code,
        ErrorCode::StoreUnavailable
    );
    assert!(!e.is_available());
    // The generation rotates once, not on every subsequent failure.
    e.invalidate();
    assert_eq!(e.store_generation(), rotated);
}

#[test]
fn enrollment_refuses_a_missing_cache_instead_of_creating_one() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    drop(store);
    let cache = state.path().join("cache.db");
    fs::remove_file(&cache).unwrap();
    assert!(Enrollment::enroll(state.path()).is_err());
    assert!(
        !cache.exists(),
        "read-only enrollment must not create a cache"
    );
}

#[test]
fn a_deadline_interrupts_a_long_read_and_the_connection_survives() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path()).unwrap();

    let started = Instant::now();
    let err = e
        .read(None, Instant::now() + Duration::from_millis(250), |c| {
            c.query_row(
                "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x<2000000000) \
                 SELECT count(*) FROM c",
                [],
                |r| r.get::<_, i64>(0),
            )
        })
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::DeadlineExceeded);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "interrupt did not stop the read"
    );

    // Measured in slice 0: an interrupted read leaves the connection immediately reusable, so a
    // cancelled request must not latch the binding.
    assert!(e.is_available());
    assert_eq!(e.current_revision(soon()).unwrap(), 1);
}

#[test]
fn a_queued_read_past_its_deadline_never_interrupts_the_running_one() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path()).unwrap());

    let holder = {
        let e = e.clone();
        thread::spawn(move || {
            e.read(None, Instant::now() + Duration::from_secs(30), |_| {
                thread::sleep(Duration::from_millis(700));
                Ok(42)
            })
        })
    };
    thread::sleep(Duration::from_millis(100));

    // Queued behind the holder with a deadline that elapses while waiting. It must be refused at
    // its own deadline rather than parked until the holder releases the connection.
    let queued_at = Instant::now();
    let queued = e.read(
        None,
        Instant::now() + Duration::from_millis(150),
        |_| Ok(()),
    );
    let waited = queued_at.elapsed();
    assert_eq!(queued.unwrap_err().code, ErrorCode::DeadlineExceeded);
    assert!(
        waited < Duration::from_millis(500),
        "the queued reader waited {waited:?}, so its own deadline did not take effect"
    );

    let (revision, value) = holder.join().unwrap().expect("holder was interrupted");
    assert_eq!((revision, value), (1, 42));
    assert!(e.is_available());
}

#[test]
fn an_elapsed_deadline_is_refused_even_when_the_connection_is_idle() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path()).unwrap();
    let ran = Arc::new(AtomicBool::new(false));
    let seen = ran.clone();

    let past = Instant::now() - Duration::from_secs(1);
    let err = e
        .read(None, past, move |_| {
            seen.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
        .unwrap_err();

    assert_eq!(err.code, ErrorCode::DeadlineExceeded);
    assert!(
        !ran.load(std::sync::atomic::Ordering::SeqCst),
        "an expired request must not reach the connection"
    );
    assert!(e.is_available());
}

#[test]
fn a_read_that_outlives_its_deadline_is_refused_even_when_it_succeeds() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path()).unwrap();

    // No SQL, so there is nothing for the watchdog to interrupt: completion is judged by the clock.
    let err = e
        .read(None, Instant::now() + Duration::from_millis(20), |_| {
            thread::sleep(Duration::from_millis(120));
            Ok(7)
        })
        .unwrap_err();

    assert_eq!(err.code, ErrorCode::DeadlineExceeded);
    // A late request is refused, not fatal: the binding stays usable.
    assert!(e.is_available());
    assert_eq!(e.current_revision(soon()).unwrap(), 1);
}

#[test]
fn a_storage_failure_is_unavailable_rather_than_an_unindexed_store() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path()).unwrap();
    assert_eq!(e.current_revision(soon()).unwrap(), 1);

    // A cache that cannot answer the revision query is broken, not empty.
    let writer = rusqlite::Connection::open(state.path().join("cache.db")).unwrap();
    writer.execute_batch("DROP TABLE revision").unwrap();
    drop(writer);

    let err = e.current_revision(soon()).unwrap_err();
    assert_eq!(err.code, ErrorCode::StoreUnavailable);
}
