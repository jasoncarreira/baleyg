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
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    assert!(e.is_available());
    assert_eq!(e.current_revision(soon()).unwrap(), 0);
}

#[test]
fn reads_the_published_revision_in_one_transaction() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
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
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
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
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
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
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
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
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
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
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
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
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
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
    let lock = store.publication_lock();
    drop(store);
    let cache = state.path().join("cache.db");
    fs::remove_file(&cache).unwrap();
    assert!(Enrollment::enroll(state.path(), lock).is_err());
    assert!(
        !cache.exists(),
        "read-only enrollment must not create a cache"
    );
}

#[test]
fn a_deadline_interrupts_a_long_read_and_the_connection_survives() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();

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
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());

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
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
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
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();

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
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    assert_eq!(e.current_revision(soon()).unwrap(), 1);

    // A cache that cannot answer the revision query is broken, not empty.
    let writer = rusqlite::Connection::open(state.path().join("cache.db")).unwrap();
    writer.execute_batch("DROP TABLE revision").unwrap();
    drop(writer);

    let err = e.current_revision(soon()).unwrap_err();
    assert_eq!(err.code, ErrorCode::StoreUnavailable);
}

#[test]
fn admission_refuses_after_another_request_observes_invalidation() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let (revision, ()) = e.read(None, soon(), |_| Ok(())).unwrap();

    // Another request observes the cache is gone and latches the binding while this response is
    // still pending.
    fs::remove_file(state.path().join("cache.db")).unwrap();
    assert_eq!(
        e.read(None, soon(), |_| Ok(())).unwrap_err().code,
        ErrorCode::StoreUnavailable
    );

    let produced = Arc::new(AtomicBool::new(false));
    let flag = produced.clone();
    let err = e
        .admit(revision, soon(), move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        })
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::StoreUnavailable);
    assert!(
        !produced.load(std::sync::atomic::Ordering::SeqCst),
        "nothing may be produced after observed invalidation"
    );
}

#[test]
fn admission_refuses_a_snapshot_a_later_publication_replaced() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let (revision, ()) = e.read(None, soon(), |_| Ok(())).unwrap();
    assert_eq!(revision, 1);

    // The owner republishes between the read and the response.
    publish(&store, Some(1));

    let err = e.admit(revision, soon(), || ()).unwrap_err();
    assert_eq!(err.code, ErrorCode::RevisionConflict);
    // A conflict is not identity loss: the binding stays usable at the new revision.
    assert!(e.is_available());
    assert!(e.admit(2, soon(), || ()).is_ok());
}

#[test]
fn an_invalidation_during_production_suppresses_the_response() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());

    // Invalidation is deliberately not serialized against admission: it must be recordable at
    // once, even while a response is in flight. The response is suppressed by the verification
    // that follows production rather than by making the observer wait.
    let (entered, inside) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let admitter = {
        let e = e.clone();
        thread::spawn(move || {
            e.admit_current(Instant::now() + Duration::from_secs(30), move |_| {
                entered.send(()).unwrap();
                wait.recv().unwrap();
                "built"
            })
        })
    };
    inside.recv().unwrap();

    let started = Instant::now();
    e.invalidate();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "invalidation waited {:?} behind an in-flight response",
        started.elapsed()
    );
    release.send(()).unwrap();

    assert_eq!(
        admitter.join().unwrap().unwrap_err().code,
        ErrorCode::StoreUnavailable,
        "a response was returned after the store was invalidated"
    );
}

#[test]
fn admission_cannot_complete_while_a_publication_is_in_flight() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());

    // Hold an admission open, then publish from another thread. Publication shares this boundary,
    // so it cannot complete while a response is being produced from the revision it replaces.
    let (entered, inside) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let admitter = {
        let e = e.clone();
        thread::spawn(move || {
            e.admit(1, Instant::now() + Duration::from_secs(30), move || {
                entered.send(()).unwrap();
                wait.recv().unwrap();
                "produced"
            })
        })
    };
    inside.recv().unwrap();

    let publisher = {
        let store = store.clone();
        thread::spawn(move || publish(&store, Some(1)))
    };
    thread::sleep(Duration::from_millis(80));
    assert!(
        !publisher.is_finished(),
        "publication completed while an admission was open"
    );
    release.send(()).unwrap();

    assert_eq!(admitter.join().unwrap().unwrap().0, "produced");
    assert_eq!(publisher.join().unwrap(), 2);
    // The next admission sees the new revision and refuses the old basis.
    assert_eq!(
        e.admit(1, soon(), || ()).unwrap_err().code,
        ErrorCode::RevisionConflict
    );
}

#[test]
fn a_negative_stored_revision_is_a_storage_failure_not_revision_zero() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();

    let writer = rusqlite::Connection::open(state.path().join("cache.db")).unwrap();
    writer
        .execute_batch("UPDATE revision SET revision=-1 WHERE singleton=1")
        .unwrap();
    drop(writer);

    assert_eq!(
        e.current_revision(soon()).unwrap_err().code,
        ErrorCode::StoreUnavailable
    );
}

#[test]
fn admission_detects_a_publication_from_another_store_handle() {
    let (state, work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    assert!(e.admit(1, soon(), || ()).is_ok());

    // A separate Store over the same directory, as a second process would have. It shares no
    // in-process state, so admission must re-read the revision rather than trust a counter.
    let other = Store::open(state.path(), work.path()).unwrap();
    publish(&other, Some(1));

    assert_eq!(
        e.admit(1, soon(), || ()).unwrap_err().code,
        ErrorCode::RevisionConflict
    );
    assert!(e.admit(2, soon(), || ()).is_ok());
}

#[test]
fn admission_is_bounded_and_does_not_report_contention_as_a_conflict() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();

    // Held the way a long publication holds it.
    let held = store.publication_lock();
    let guard = held.publish().unwrap();

    let started = Instant::now();
    let err = e
        .admit(1, Instant::now() + Duration::from_millis(120), || ())
        .unwrap_err();
    let waited = started.elapsed();

    // Contention is an elapsed deadline, never a false revision conflict.
    assert_eq!(err.code, ErrorCode::DeadlineExceeded);
    assert!(waited < Duration::from_secs(2), "waited {waited:?}");
    drop(guard);
    assert!(e.admit(1, soon(), || ()).is_ok());
}

#[test]
fn a_failed_read_still_latches_an_identity_loss() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();

    // Replace the cache while a read is failing for an unrelated reason: the identity check must
    // still run, or the binding would be left available for the next request.
    replace_with_new_inode(state.path(), "cache.db");
    let err = e.read(Some(99), soon(), |_| Ok(())).unwrap_err();
    assert_eq!(err.code, ErrorCode::StoreUnavailable);
    assert!(
        !e.is_available(),
        "identity loss was not latched on a failing read"
    );
}

#[test]
fn admission_discards_a_value_built_over_a_replaced_snapshot() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());
    let path = state.path().to_path_buf();

    // Publication from any handle now waits for the in-flight response, so the case that can still
    // land during production is an external replacement of the cache, which no lock can hold off.
    let (entered, inside) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let admitter = {
        let e = e.clone();
        thread::spawn(move || {
            e.admit_current(Instant::now() + Duration::from_secs(30), move |revision| {
                entered.send(revision).unwrap();
                wait.recv().unwrap();
                "built"
            })
        })
    };
    assert_eq!(inside.recv().unwrap(), 1);
    replace_with_new_inode(&path, "cache.db");
    release.send(()).unwrap();

    assert_eq!(
        admitter.join().unwrap().unwrap_err().code,
        ErrorCode::StoreUnavailable,
        "a value built over a replaced snapshot was returned"
    );
}

#[test]
fn admission_latches_an_identity_loss_that_lands_while_producing() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());
    let path = state.path().to_path_buf();

    let (entered, inside) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let admitter = {
        let e = e.clone();
        thread::spawn(move || {
            e.admit_current(Instant::now() + Duration::from_secs(30), move |_| {
                entered.send(()).unwrap();
                wait.recv().unwrap();
            })
        })
    };
    inside.recv().unwrap();
    replace_with_new_inode(&path, "cache.db");
    release.send(()).unwrap();

    assert_eq!(
        admitter.join().unwrap().unwrap_err().code,
        ErrorCode::StoreUnavailable
    );
    assert!(
        !e.is_available(),
        "identity loss during production was not latched"
    );
}

#[test]
fn an_already_elapsed_deadline_is_refused_even_on_a_free_boundary() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let ran = Arc::new(AtomicBool::new(false));
    let seen = ran.clone();

    let err = e
        .admit_current(Instant::now() - Duration::from_secs(1), move |_| {
            seen.store(true, std::sync::atomic::Ordering::SeqCst);
        })
        .unwrap_err();

    assert_eq!(err.code, ErrorCode::DeadlineExceeded);
    assert!(
        !ran.load(std::sync::atomic::Ordering::SeqCst),
        "an expired admission still reached its producer"
    );
}

#[test]
fn a_producer_that_overruns_the_deadline_is_refused() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();

    let err = e
        .admit_current(Instant::now() + Duration::from_millis(20), |_| {
            thread::sleep(Duration::from_millis(120));
        })
        .unwrap_err();

    assert_eq!(err.code, ErrorCode::DeadlineExceeded);
    assert!(e.is_available(), "an overrun is not identity loss");
}

#[test]
fn publication_waits_for_an_admitted_response_to_be_emitted() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();

    // The ticket represents a response that has been admitted but not yet handed to the server.
    let (_value, ticket) = e.admit(1, soon(), || "evidence").unwrap();

    // Publication through any handle sharing this boundary -- every clone of this Store, which is
    // what the daemon and its indexer use -- waits rather than landing between admission and
    // emission. A separate process has its own boundary and can only be detected, not held off.
    let same = store.clone();
    let (started, running) = std::sync::mpsc::channel();
    let publisher = thread::spawn(move || {
        // Signalled before the call, so the assertion below cannot pass merely because the thread
        // had not begun.
        started.send(()).unwrap();
        publish(&same, Some(1))
    });
    running.recv().unwrap();
    thread::sleep(Duration::from_millis(80));
    assert!(
        !publisher.is_finished(),
        "publication completed while an admitted response had not been emitted"
    );

    drop(ticket);
    assert_eq!(publisher.join().unwrap(), 2);
}

#[test]
fn a_producer_effect_is_undone_when_verification_fails() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());
    let path = state.path().to_path_buf();

    // Identity loss, not publication: a publication from any handle now waits for the ticket, so
    // replacement is what can still land while a producer runs.
    let undone = Arc::new(AtomicBool::new(false));
    let flag = undone.clone();
    let (entered, inside) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let admitter = {
        let e = e.clone();
        thread::spawn(move || {
            e.admit_with(
                Instant::now() + Duration::from_secs(30),
                move |_| {
                    entered.send(()).unwrap();
                    wait.recv().unwrap();
                    ("value", "effect-handle")
                },
                move |_handle, _e| flag.store(true, std::sync::atomic::Ordering::SeqCst),
            )
        })
    };
    inside.recv().unwrap();
    replace_with_new_inode(&path, "cache.db");
    release.send(()).unwrap();
    let outcome = admitter.join().unwrap();

    assert!(
        outcome.is_err(),
        "a value built over a replaced snapshot was returned"
    );
    assert!(
        undone.load(std::sync::atomic::Ordering::SeqCst),
        "the producer's effect was not undone"
    );
}

#[test]
fn a_second_store_cannot_publish_while_a_response_is_in_flight() {
    let (state, work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let (_value, ticket) = e.admit(1, soon(), || "evidence").unwrap();

    // A separately opened Store shares no in-process state, exactly as a standalone `baleyg index`
    // process would not. The advisory lock in the state directory is what holds it off.
    let other = Store::open(state.path(), work.path()).unwrap();
    let (started, running) = std::sync::mpsc::channel();
    let publisher = thread::spawn(move || {
        started.send(()).unwrap();
        publish(&other, Some(1))
    });
    running.recv().unwrap();
    thread::sleep(Duration::from_millis(150));
    assert!(
        !publisher.is_finished(),
        "a separate Store published between admission and emission"
    );

    drop(ticket);
    assert_eq!(publisher.join().unwrap(), 2);
}

#[test]
fn a_pending_ticket_never_blocks_an_invalidation_observer() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());

    // A response admitted but not yet emitted.
    let (_value, ticket) = e.admit(1, soon(), || "evidence").unwrap();

    // Another request observes the cache is gone. It must be able to record that at once: making
    // it wait for the in-flight response would leave the binding reporting itself available while
    // a stale body was still emit-ready.
    fs::remove_file(state.path().join("cache.db")).unwrap();
    let observer = {
        let e = e.clone();
        thread::spawn(move || e.current_revision(Instant::now() + Duration::from_secs(5)))
    };
    let started = Instant::now();
    let outcome = observer.join().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the observer waited {:?} behind a pending ticket",
        started.elapsed()
    );
    assert_eq!(outcome.unwrap_err().code, ErrorCode::StoreUnavailable);
    assert!(
        !e.is_available(),
        "identity loss was not recorded while a ticket was held"
    );

    // And the admitted response is suppressed rather than emitted.
    assert_eq!(
        ticket.commit(&e).unwrap_err().code,
        ErrorCode::StoreUnavailable
    );
}

#[test]
fn publication_is_not_starved_by_a_stream_of_admissions() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());

    let stop = Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..3)
        .map(|_| {
            let (e, stop) = (e.clone(), stop.clone());
            thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    let _ = e.admit_current(Instant::now() + Duration::from_secs(5), |_| ());
                }
            })
        })
        .collect();

    let started = Instant::now();
    let revision = publish(&store, Some(1));
    let waited = started.elapsed();
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    for r in readers {
        r.join().unwrap();
    }

    assert_eq!(revision, 2);
    assert!(
        waited < Duration::from_secs(5),
        "publication waited {waited:?} behind continuous admissions"
    );
}

#[test]
fn commit_and_invalidation_cannot_interleave() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());
    let (_value, ticket) = e.admit(1, soon(), || "evidence").unwrap();

    // The commit and the invalidation take the same lock, so they land on one side or the other.
    // A separate check followed by a release would leave a window between them in which the
    // response is already cleared but not yet handed off.
    let racer = {
        let e = e.clone();
        thread::spawn(move || e.invalidate())
    };
    let committed = ticket.commit(&e).is_ok();
    racer.join().unwrap();

    // Either ordering is permitted; what must not happen is a commit that succeeds while the
    // store is already latched, so a refused commit implies the latch was observed.
    assert!(!e.is_available());
    if !committed {
        // Refused, which is the only outcome once the latch is visible to the commit.
    }

    // And once latched, nothing admitted afterwards can ever commit.
    match e.admit(1, soon(), || "evidence") {
        Ok((_v, later)) => assert_eq!(
            later.commit(&e).unwrap_err().code,
            ErrorCode::StoreUnavailable
        ),
        Err(err) => assert_eq!(err.code, ErrorCode::StoreUnavailable),
    }
}

#[test]
fn an_unusable_lock_file_refuses_admission_rather_than_dropping_coordination() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    assert!(e.admit(1, soon(), || ()).is_ok());

    // Replace the lock file with a directory so it cannot be opened. Continuing without the lock
    // would hand out a ticket unable to hold off a separate Store, so admission must refuse.
    let lock = state.path().join("publication.lock");
    fs::remove_file(&lock).ok();
    fs::create_dir(&lock).unwrap();

    assert_eq!(
        e.admit(1, soon(), || ()).unwrap_err().code,
        ErrorCode::StoreUnavailable,
        "admission continued without cross-process coordination"
    );
}
