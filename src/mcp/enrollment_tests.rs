//! Slice 1: the enrolled read-only connection and its identity guard.
//!
//! Covers acceptance test 5 (identity) and the read-only WAL qualification in test 7.
use crate::{
    mcp::{ErrorCode, enrollment::Enrollment},
    model::*,
    store::Store,
};
use std::{
    fs,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

fn run_async<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn commit_pending<T: Send + 'static>(
    pending: crate::mcp::enrollment::Pending<T>,
) -> Result<T, crate::mcp::McpError> {
    run_async(async { pending.prepare_handoff().await?.commit().await })
}

fn invalidate(enrollment: &Enrollment) {
    run_async(enrollment.invalidate(Instant::now() + Duration::from_secs(5))).unwrap();
}

fn commit_with_pending<T, Plan, Prepare, Finalize>(
    pending: crate::mcp::enrollment::Pending<T>,
    prepare: Prepare,
    finalize: Finalize,
) -> Result<T, crate::mcp::McpError>
where
    T: Send + 'static,
    Prepare: for<'a> FnOnce(
            &crate::mcp::enrollment::HandoffView<'a>,
        ) -> Result<Plan, crate::mcp::McpError>
        + Send,
    Finalize: for<'a> FnOnce(crate::mcp::enrollment::LifecycleCommit<'a>, Plan) + Send,
{
    run_async(async {
        pending
            .prepare_handoff()
            .await?
            .commit_with(prepare, finalize)
            .await
    })
}

#[derive(Debug)]
struct DropSignal(Arc<AtomicBool>);
impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

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
fn enrollment_rejects_a_boundary_for_another_state_directory() {
    let (state_a, _work_a, _store_a) = fixture();
    let (_state_b, _work_b, store_b) = fixture();
    let error = match Enrollment::enroll(state_a.path(), store_b.publication_lock()) {
        Ok(_) => panic!("enrollment accepted a boundary from another state"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("publication boundary belongs to a different state directory")
    );
}

#[test]
fn reads_the_published_revision_in_one_transaction() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let snapshot = e
        .read(None, soon(), |c| {
            let mut stmt = c.prepare("SELECT id FROM nodes ORDER BY id")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    let pending = e
        .admit_snapshot(snapshot, |revision, names| (revision, names))
        .unwrap();
    let (revision, names) = commit_pending(pending).unwrap();
    assert_eq!(revision, 1);
    assert_eq!(names, vec!["a".to_string()]);
}

#[test]
fn snapshot_proof_rejects_a_different_enrollment() {
    let (state_a, _work_a, store_a) = fixture();
    let (state_b, _work_b, store_b) = fixture();
    publish(&store_a, Some(0));
    publish(&store_b, Some(0));
    let a = Enrollment::enroll(state_a.path(), store_a.publication_lock()).unwrap();
    let b = Enrollment::enroll(state_b.path(), store_b.publication_lock()).unwrap();
    let proof = a.read(None, soon(), |_| Ok("evidence")).unwrap();
    assert_eq!(
        b.admit_snapshot(proof, |_, value| value).unwrap_err().code,
        ErrorCode::BindingMismatch
    );
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
    invalidate(&e);
    let rotated = e.store_generation();

    // Browser or owner activity that republishes the index must not restore MCP availability.
    publish(&store, Some(1));

    assert_eq!(
        e.read(None, soon(), |_| Ok(())).unwrap_err().code,
        ErrorCode::StoreUnavailable
    );
    assert!(!e.is_available());
    // The generation rotates once, not on every subsequent failure.
    invalidate(&e);
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

    let snapshot = holder.join().unwrap().expect("holder was interrupted");
    let pending = e
        .admit_snapshot(snapshot, |revision, value| (revision, value))
        .unwrap();
    assert_eq!(commit_pending(pending).unwrap(), (1, 42));
    assert!(e.is_available());
}

#[test]
fn admission_queued_for_the_connection_obeys_its_own_deadline() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = {
        let e = e.clone();
        thread::spawn(move || {
            e.read(None, soon(), |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
        })
    };
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let started = Instant::now();
    let error = e
        .admit_current(Instant::now() + Duration::from_millis(120), |_| "late")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::DeadlineExceeded);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "admission waited beyond its absolute deadline"
    );
    release_tx.send(()).unwrap();
    holder.join().unwrap().unwrap();
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
    e.read(None, soon(), |_| Ok(())).unwrap();

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
        .admit_current(soon(), move |_| {
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
    let snapshot = e.read(None, soon(), |_| Ok(())).unwrap();

    // The owner republishes between the read and the response.
    publish(&store, Some(1));

    // The proof owns revision 1, so it cannot be associated with the later revision 2.
    assert_eq!(
        e.admit_snapshot(snapshot, |_, ()| ()).unwrap_err().code,
        ErrorCode::RevisionConflict
    );
    let pending = e.admit_current(soon(), |current| current).unwrap();
    assert_eq!(commit_pending(pending).unwrap(), 2);
    assert!(e.is_available());
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
    invalidate(&e);
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
            e.admit_current(Instant::now() + Duration::from_secs(30), move |_| {
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

    assert_eq!(
        commit_pending(admitter.join().unwrap().unwrap()).unwrap(),
        "produced"
    );
    assert_eq!(publisher.join().unwrap(), 2);
    // The next admission owns and reports the new revision; callers cannot attach a stale basis.
    let next = e.admit_current(soon(), |revision| revision).unwrap();
    assert_eq!(commit_pending(next).unwrap(), 2);
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
    assert!(e.admit_current(soon(), |_| ()).is_ok());

    // A separate Store over the same directory, as a second process would have. It shares no
    // in-process state, so admission must re-read the revision rather than trust a counter.
    let other = Store::open(state.path(), work.path()).unwrap();
    publish(&other, Some(1));

    let pending = e.admit_current(soon(), |revision| revision).unwrap();
    assert_eq!(commit_pending(pending).unwrap(), 2);
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
        .admit_current(Instant::now() + Duration::from_millis(120), |_| ())
        .unwrap_err();
    let waited = started.elapsed();

    // Contention is an elapsed deadline, never a false revision conflict.
    assert_eq!(err.code, ErrorCode::DeadlineExceeded);
    assert!(waited < Duration::from_secs(2), "waited {waited:?}");
    drop(guard);
    assert!(e.admit_current(soon(), |_| ()).is_ok());
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

    // The pending value represents a response that has been admitted but not yet handed to the server.
    let pending = e.admit_current(soon(), |_| "evidence").unwrap();

    // Publication through any handle sharing this boundary -- every clone of this Store, which is
    // what the daemon and its indexer use -- waits rather than landing between admission and
    // emission. A cooperating separate process is held off by the same stable flock inode.
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

    assert_eq!(commit_pending(pending).unwrap(), "evidence");
    assert_eq!(publisher.join().unwrap(), 2);
}

#[test]
fn a_producer_effect_is_undone_when_verification_fails() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());
    let path = state.path().to_path_buf();

    // Identity loss, not publication: a publication from any handle now waits for the pending handoff, so
    // replacement is what can still land while a producer runs.
    let undone = Arc::new(AtomicBool::new(false));
    let flag = undone.clone();
    let (entered, inside) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let admitter = {
        let e = e.clone();
        thread::spawn(move || {
            e.admit_current(Instant::now() + Duration::from_secs(30), move |_| {
                entered.send(()).unwrap();
                wait.recv().unwrap();
                DropSignal(flag)
            })
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
    let pending = e.admit_current(soon(), |_| "evidence").unwrap();

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

    assert_eq!(commit_pending(pending).unwrap(), "evidence");
    assert_eq!(publisher.join().unwrap(), 2);
}

#[test]
fn a_pending_handoff_never_blocks_an_invalidation_observer() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());

    // A response admitted but not yet emitted.
    let pending = e.admit_current(soon(), |_| "evidence").unwrap();

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
        "the observer waited {:?} behind a pending handoff",
        started.elapsed()
    );
    assert_eq!(outcome.unwrap_err().code, ErrorCode::StoreUnavailable);
    assert!(
        !e.is_available(),
        "identity loss was not recorded while a pending handoff was held"
    );

    // And the admitted response is suppressed rather than emitted.
    assert_eq!(
        commit_pending(pending).unwrap_err().code,
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
    let pending = e.admit_current(soon(), |_| "evidence").unwrap();

    // The commit and the invalidation take the same lock, so they land on one side or the other.
    // A separate check followed by a release would leave a window between them in which the
    // response is already cleared but not yet handed off.
    let racer = {
        let e = e.clone();
        thread::spawn(move || invalidate(&e))
    };
    let committed = commit_pending(pending).is_ok();
    racer.join().unwrap();

    // Either ordering is permitted; what must not happen is a commit that succeeds while the
    // store is already latched, so a refused commit implies the latch was observed.
    assert!(!e.is_available());
    if !committed {
        // Refused, which is the only outcome once the latch is visible to the commit.
    }

    // And once latched, nothing admitted afterwards can ever commit.
    match e.admit_current(soon(), |_| "evidence") {
        Ok(later) => assert_eq!(
            commit_pending(later).unwrap_err().code,
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
    assert!(e.admit_current(soon(), |_| ()).is_ok());

    // Replace the lock file with a directory so it cannot be opened. Continuing without the lock
    // would hand out a pending handoff unable to hold off a separate Store, so admission must refuse.
    let lock = state.path().join("publication.lock");
    fs::remove_file(&lock).ok();
    fs::create_dir(&lock).unwrap();

    assert_eq!(
        e.admit_current(soon(), |_| ()).unwrap_err().code,
        ErrorCode::StoreUnavailable,
        "admission continued without cross-process coordination"
    );
}

#[test]
fn final_commit_uses_the_original_deadline() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let deadline = Instant::now() + Duration::from_millis(40);
    let pending = e.admit_current(deadline, |_| "late").unwrap();
    while Instant::now() < deadline {
        thread::yield_now();
    }

    assert_eq!(
        commit_pending(pending).unwrap_err().code,
        ErrorCode::DeadlineExceeded
    );
    // The failed commit released both sides of the publication boundary.
    assert_eq!(publish(&store, Some(1)), 2);
}

#[test]
fn dropping_the_last_prepared_clone_releases_inert_value_and_publication() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let cleaned = Arc::new(AtomicBool::new(false));
    let pending = e
        .admit_current(soon(), |_| DropSignal(cleaned.clone()))
        .unwrap();
    let prepared = run_async(pending.prepare_handoff()).unwrap();
    let extension_clone = prepared.clone();
    drop(prepared);
    assert!(!cleaned.load(Ordering::SeqCst));

    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let publisher = thread::spawn(move || {
        started_tx.send(()).unwrap();
        let revision = publish(&store, Some(1));
        done_tx.send(revision).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());

    drop(extension_clone);
    assert!(cleaned.load(Ordering::SeqCst));
    assert_eq!(done_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 2);
    publisher.join().unwrap();
}

#[test]
fn commit_with_validation_and_invalidation_have_one_order() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());
    let generation = e.store_generation();
    let pending = e.admit_current(soon(), |_| "response").unwrap();
    let (validating_tx, validating_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let committer = thread::spawn(move || {
        commit_with_pending(
            pending,
            move |view| {
                assert_eq!(view.lifecycle().store_generation(), generation);
                validating_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            },
            |_, ()| {},
        )
    });
    validating_rx.recv().unwrap();

    let (attempting_tx, attempting_rx) = std::sync::mpsc::channel();
    let (invalidated_tx, invalidated_rx) = std::sync::mpsc::channel();
    let invalidator = {
        let e = e.clone();
        thread::spawn(move || {
            attempting_tx.send(()).unwrap();
            invalidate(&e);
            invalidated_tx.send(()).unwrap();
        })
    };
    attempting_rx.recv().unwrap();
    assert!(
        invalidated_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "invalidation interleaved with commit validation"
    );
    release_tx.send(()).unwrap();

    assert_eq!(committer.join().unwrap().unwrap(), "response");
    invalidated_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    invalidator.join().unwrap();
    assert!(!e.is_available());
}

#[test]
fn final_commit_requires_the_original_admitted_revision() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let pending = e.admit_current(soon(), |_| "stale").unwrap();

    // This deliberately bypasses Store and its advisory boundary to qualify the final sampled
    // revision check against an unsupported writer that preserves every file identity.
    let db = rusqlite::Connection::open(state.path().join("cache.db")).unwrap();
    db.execute("UPDATE revision SET revision=2 WHERE singleton=1", [])
        .unwrap();
    drop(db);

    let error = commit_pending(pending).unwrap_err();
    assert_eq!(error.code, ErrorCode::RevisionConflict);
}

#[test]
fn final_commit_queued_for_the_connection_obeys_its_deadline() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());
    let deadline = Instant::now() + Duration::from_millis(250);
    let pending = e.admit_current(deadline, |_| "response").unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = {
        let e = e.clone();
        thread::spawn(move || {
            e.read(None, soon(), |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
        })
    };
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let started = Instant::now();
    let error = commit_pending(pending).unwrap_err();
    assert_eq!(error.code, ErrorCode::DeadlineExceeded);
    assert!(started.elapsed() < Duration::from_secs(1));
    release_tx.send(()).unwrap();
    holder.join().unwrap().unwrap();
    assert!(e.is_available());
}

#[test]
fn final_commit_rechecks_identity_after_a_paused_validator() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let pending = e
        .admit_current(Instant::now() + Duration::from_secs(5), |_| "response")
        .unwrap();
    let lock = state.path().join("publication.lock");
    let replacement = state.path().join("replacement.lock");
    fs::write(&replacement, b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600)).unwrap();
    }

    let error = commit_with_pending(
        pending,
        |_| {
            fs::rename(&replacement, &lock).unwrap();
            Err::<(), _>(crate::mcp::McpError::new(
                ErrorCode::Unauthorized,
                "unrelated prepare error",
            ))
        },
        |_, ()| {},
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::StoreUnavailable);
    assert!(!e.is_available());
}

#[test]
fn a_new_store_on_a_replacement_lock_cannot_validate_an_old_pending_value() {
    let (state, work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let pending = e.admit_current(soon(), |_| "stale").unwrap();
    let lock = state.path().join("publication.lock");
    fs::remove_file(&lock).unwrap();
    fs::write(&lock, b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
    }

    let replacement_store = Store::open(state.path(), work.path()).unwrap();
    assert_eq!(publish(&replacement_store, Some(1)), 2);
    let error = commit_pending(pending).unwrap_err();
    assert_eq!(error.code, ErrorCode::StoreUnavailable);
}

#[test]
fn lock_path_mutations_fail_closed() {
    #[derive(Clone, Copy)]
    enum Mutation {
        Missing,
        Replacement,
        Symlink,
        Hardlink,
        Mode,
    }
    for mutation in [
        Mutation::Missing,
        Mutation::Replacement,
        Mutation::Symlink,
        Mutation::Hardlink,
        Mutation::Mode,
    ] {
        let (state, _work, store) = fixture();
        publish(&store, Some(0));
        let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
        let lock = state.path().join("publication.lock");
        match mutation {
            Mutation::Missing => fs::remove_file(&lock).unwrap(),
            Mutation::Replacement => {
                fs::remove_file(&lock).unwrap();
                fs::write(&lock, b"replacement").unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
                }
            }
            Mutation::Symlink => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::symlink;
                    let target = state.path().join("target.lock");
                    fs::write(&target, b"").unwrap();
                    fs::remove_file(&lock).unwrap();
                    symlink(&target, &lock).unwrap();
                }
            }
            Mutation::Hardlink => {
                fs::hard_link(&lock, state.path().join("second-link")).unwrap();
            }
            Mutation::Mode => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&lock, fs::Permissions::from_mode(0o640)).unwrap();
                }
            }
        }
        let error = e.admit_current(soon(), |_| "response").unwrap_err();
        assert_eq!(error.code, ErrorCode::StoreUnavailable);
        assert!(!e.is_available());
        assert!(
            store
                .publish(
                    &graph(),
                    Some(1),
                    &(Arc::new(AtomicBool::new(false)) as CancelFlag),
                )
                .is_err(),
            "publication accepted a mutated lock path"
        );
    }
}

#[test]
fn validator_delay_past_the_original_deadline_refuses_handoff() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let deadline = Instant::now() + Duration::from_millis(150);
    let cleaned = Arc::new(AtomicBool::new(false));
    let pending = e
        .admit_current(deadline, |_| ("response", DropSignal(cleaned.clone())))
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let committer = thread::spawn(move || {
        commit_with_pending(
            pending,
            move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            },
            |_, ()| {},
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    thread::sleep(deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(20));
    release_tx.send(()).unwrap();
    let error = committer.join().unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::DeadlineExceeded);
    assert!(cleaned.load(Ordering::SeqCst));
}

#[test]
fn callback_panics_invalidate_without_poison_recovery() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let pending = e.admit_current(soon(), |_| "response").unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = commit_with_pending(
            pending,
            |_| -> Result<(), crate::mcp::McpError> { panic!("validator panic") },
            |_, ()| {},
        );
    }));
    assert!(panic.is_err());
    assert!(!e.is_available());

    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = e.read(None, soon(), |_| -> rusqlite::Result<()> {
            panic!("reader panic")
        });
    }));
    assert!(panic.is_err());
    assert!(!e.is_available());

    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = e.admit_current::<()>(soon(), |_| panic!("producer panic"));
    }));
    assert!(panic.is_err());
    assert!(!e.is_available());
}

#[test]
fn finalizer_panic_child_helper() {
    if std::env::var_os("BALEYG_FINALIZER_PANIC_CHILD").is_none() {
        return;
    }
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let pending = e.admit_current(soon(), |_| "response").unwrap();
    let _ = commit_with_pending(
        pending,
        |_| Ok(()),
        |_, ()| panic!("partial authority mutation"),
    );
    panic!("finalizer panic unexpectedly returned");
}

#[test]
fn finalizer_panic_is_process_fatal() {
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("mcp::enrollment_tests::finalizer_panic_child_helper")
        .arg("--nocapture")
        .env("BALEYG_FINALIZER_PANIC_CHILD", "1")
        .status()
        .unwrap();
    assert!(!status.success(), "a finalizer panic did not fail-stop");
}

#[test]
fn rollback_failure_never_returns_evidence_and_latches() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let error = e
        .read(None, soon(), |conn| {
            conn.execute_batch("ROLLBACK")?;
            Ok("must not escape")
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::StoreUnavailable);
    assert!(!e.is_available());
}

#[test]
fn ambiguous_commit_failure_keeps_publication_fail_closed() {
    let (state, work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let db = rusqlite::Connection::open(state.path().join("cache.db")).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    db.execute_batch(
        "CREATE TABLE commit_failure(path TEXT REFERENCES files(path) DEFERRABLE INITIALLY DEFERRED);
         CREATE TRIGGER fail_at_commit AFTER INSERT ON revision BEGIN
             INSERT INTO commit_failure VALUES('missing.js');
         END;",
    )
    .unwrap();
    drop(db);

    assert!(
        store
            .publish(
                &graph(),
                Some(1),
                &(Arc::new(AtomicBool::new(false)) as CancelFlag),
            )
            .is_err()
    );
    assert!(
        store
            .publish(
                &graph(),
                None,
                &(Arc::new(AtomicBool::new(false)) as CancelFlag),
            )
            .is_err()
    );
    assert_eq!(
        e.admit_current(soon(), |_| "response").unwrap_err().code,
        ErrorCode::StoreUnavailable
    );
    assert!(
        Store::open(state.path(), work.path()).is_err(),
        "a new Store in the same process must not re-anchor after ambiguous commit"
    );
}

#[test]
fn known_precommit_error_reopens_publication() {
    let (_state, _work, store) = fixture();
    publish(&store, Some(0));
    assert!(
        store
            .publish(
                &graph(),
                Some(99),
                &(Arc::new(AtomicBool::new(false)) as CancelFlag),
            )
            .is_err()
    );
    assert_eq!(publish(&store, Some(1)), 2);
}

#[test]
fn flock_publication_child_helper() {
    let Ok(state) = std::env::var("BALEYG_FLOCK_CHILD_STATE") else {
        return;
    };
    let work = std::env::var("BALEYG_FLOCK_CHILD_WORK").unwrap();
    let attempted = std::env::var("BALEYG_FLOCK_CHILD_ATTEMPTED").unwrap();
    let completed = std::env::var("BALEYG_FLOCK_CHILD_COMPLETED").unwrap();
    let store = Store::open(std::path::Path::new(&state), std::path::Path::new(&work)).unwrap();

    // This nonblocking probe is the authoritative handshake: the child has actually attempted the
    // exclusive OS lock and observed the parent's shared lease, rather than merely reaching a line
    // immediately before `publish`.
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(std::path::Path::new(&state).join("publication.lock"))
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, -1, "child unexpectedly acquired the exclusive flock");
        assert_eq!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    drop(file);
    fs::write(attempted, b"blocked").unwrap();
    assert_eq!(publish(&store, Some(1)), 2);
    fs::write(completed, b"complete").unwrap();
}

#[test]
fn a_real_child_process_cannot_publish_before_commit() {
    let (state, work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let pending = e.admit_current(soon(), |_| "response").unwrap();
    let attempted = state.path().join("child-attempted");
    let completed = state.path().join("child-completed");

    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("mcp::enrollment_tests::flock_publication_child_helper")
        .arg("--nocapture")
        .env("BALEYG_FLOCK_CHILD_STATE", state.path())
        .env("BALEYG_FLOCK_CHILD_WORK", work.path())
        .env("BALEYG_FLOCK_CHILD_ATTEMPTED", &attempted)
        .env("BALEYG_FLOCK_CHILD_COMPLETED", &completed)
        .spawn()
        .unwrap();
    let attempted_deadline = Instant::now() + Duration::from_secs(5);
    while !attempted.exists() && Instant::now() < attempted_deadline {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("child exited before the blocked-lock handshake: {status}");
        }
        thread::yield_now();
    }
    if !attempted.exists() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("child did not prove exclusive-lock contention");
    }

    let exclusion_deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < exclusion_deadline {
        assert!(
            !completed.exists(),
            "child published before response commit"
        );
        assert!(
            child.try_wait().unwrap().is_none(),
            "child exited while excluded"
        );
        thread::yield_now();
    }

    assert_eq!(commit_pending(pending).unwrap(), "response");
    let completion_deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= completion_deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child did not complete after response commit");
        }
        thread::yield_now();
    };
    assert!(status.success());
    assert!(completed.exists());
    assert_eq!(store.status().unwrap().revision, 2);
}

#[test]
fn an_unusable_lock_file_also_refuses_publication() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let lock = state.path().join("publication.lock");
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();

    assert!(
        store
            .publish(
                &graph(),
                Some(1),
                &(Arc::new(AtomicBool::new(false)) as CancelFlag),
            )
            .is_err(),
        "publication continued without the exclusive cross-process lock"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn prepare_handoff_runs_connection_wait_off_the_async_executor() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());
    let pending = e
        .admit_current(Instant::now() + Duration::from_secs(5), |_| "response")
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = {
        let e = e.clone();
        thread::spawn(move || {
            e.read(None, soon(), |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
        })
    };
    entered_rx.recv().unwrap();

    let prepare = tokio::spawn(pending.prepare_handoff());
    let probe = tokio::spawn(async {
        tokio::task::yield_now().await;
        7
    });
    assert_eq!(
        probe.await.unwrap(),
        7,
        "prepare blocked the runtime worker"
    );
    release_tx.send(()).unwrap();
    let prepared = prepare.await.unwrap().unwrap();
    assert_eq!(prepared.commit().await.unwrap(), "response");
    holder.join().unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifecycle_wait_is_async_and_bounded_by_the_original_deadline() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());
    let deadline = Instant::now() + Duration::from_millis(180);
    let prepared = e
        .admit_current(deadline, |_| "response")
        .unwrap()
        .prepare_handoff()
        .await
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = {
        let e = e.clone();
        tokio::spawn(async move {
            e.lifecycle_transition(
                Instant::now() + Duration::from_secs(5),
                move |_| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                },
                |_, ()| (),
            )
            .await
        })
    };
    entered_rx.recv().unwrap();
    let probe = tokio::spawn(async { 11 });
    assert_eq!(
        probe.await.unwrap(),
        11,
        "lifecycle wait blocked Future::poll"
    );
    let error = prepared.commit().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::DeadlineExceeded);
    release_tx.send(()).unwrap();
    holder.await.unwrap().unwrap();
}

#[test]
fn delayed_prepare_error_yields_to_deadline_and_never_finalizes() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let deadline = Instant::now() + Duration::from_millis(120);
    let pending = e.admit_current(deadline, |_| "response").unwrap();
    let finalized = Arc::new(AtomicBool::new(false));
    let finalized_after = finalized.clone();
    let error = commit_with_pending(
        pending,
        move |_| {
            thread::sleep(
                deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(10),
            );
            Err::<(), _>(crate::mcp::McpError::new(ErrorCode::Unauthorized, "denied"))
        },
        move |_, ()| finalized_after.store(true, Ordering::SeqCst),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::DeadlineExceeded);
    assert!(!finalized.load(Ordering::SeqCst));
}

#[test]
fn lifecycle_revoke_and_prepared_commit_have_both_serial_orders() {
    for revoke_first in [true, false] {
        let (state, _work, store) = fixture();
        publish(&store, Some(0));
        let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
        let live = Arc::new(AtomicBool::new(true));
        let settled = Arc::new(AtomicBool::new(false));
        let prepared = run_async(
            e.admit_current(soon(), |_| "response")
                .unwrap()
                .prepare_handoff(),
        )
        .unwrap();
        if revoke_first {
            let live_finalize = live.clone();
            run_async(e.lifecycle_transition(
                soon(),
                |_| Ok(()),
                move |_, ()| live_finalize.store(false, Ordering::SeqCst),
            ))
            .unwrap();
        }
        let live_prepare = live.clone();
        let settled_finalize = settled.clone();
        let result = run_async(prepared.commit_with(
            move |_| {
                if live_prepare.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err(crate::mcp::McpError::new(
                        ErrorCode::Unauthorized,
                        "revoked",
                    ))
                }
            },
            move |_, ()| settled_finalize.store(true, Ordering::SeqCst),
        ));
        if revoke_first {
            assert_eq!(result.unwrap_err().code, ErrorCode::Unauthorized);
            assert!(!settled.load(Ordering::SeqCst));
        } else {
            assert_eq!(result.unwrap(), "response");
            assert!(settled.load(Ordering::SeqCst));
            let live_finalize = live.clone();
            run_async(e.lifecycle_transition(
                soon(),
                |_| Ok(()),
                move |_, ()| live_finalize.store(false, Ordering::SeqCst),
            ))
            .unwrap();
        }
        assert!(!live.load(Ordering::SeqCst));
    }
}

#[test]
fn pending_and_prepared_leases_release_publication_at_deadline() {
    enum Lease {
        Pending(crate::mcp::enrollment::Pending<&'static str>),
        Prepared(crate::mcp::enrollment::Prepared<&'static str>),
    }
    for prepare in [false, true] {
        let (state, _work, store) = fixture();
        publish(&store, Some(0));
        let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
        let deadline = Instant::now() + Duration::from_millis(120);
        let pending = e.admit_current(deadline, |_| "response").unwrap();
        let lease = if prepare {
            Lease::Prepared(run_async(pending.prepare_handoff()).unwrap())
        } else {
            Lease::Pending(pending)
        };
        let started = Instant::now();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let publisher = thread::spawn(move || {
            let revision = publish(&store, Some(1));
            done_tx.send(revision).unwrap();
        });
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        assert!(started.elapsed() >= Duration::from_millis(80));
        match lease {
            Lease::Prepared(prepared) => assert_eq!(
                run_async(prepared.commit()).unwrap_err().code,
                ErrorCode::DeadlineExceeded
            ),
            Lease::Pending(pending) => assert_eq!(
                run_async(pending.prepare_handoff()).unwrap_err().code,
                ErrorCode::DeadlineExceeded
            ),
        }
        publisher.join().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canceling_a_lifecycle_wait_drops_inert_state_without_finalizing() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());
    let prepared = e
        .admit_current(Instant::now() + Duration::from_secs(5), |_| "response")
        .unwrap()
        .prepare_handoff()
        .await
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = {
        let e = e.clone();
        tokio::spawn(async move {
            e.lifecycle_transition(
                soon(),
                move |_| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                },
                |_, ()| (),
            )
            .await
        })
    };
    entered_rx.recv().unwrap();
    let finalized = Arc::new(AtomicBool::new(false));
    let finalized_after = finalized.clone();
    let committing = tokio::spawn(prepared.commit_with(
        |_| Ok(()),
        move |_, ()| finalized_after.store(true, Ordering::SeqCst),
    ));
    tokio::task::yield_now().await;
    committing.abort();
    assert!(committing.await.unwrap_err().is_cancelled());

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let publisher = thread::spawn(move || {
        done_tx.send(publish(&store, Some(1))).unwrap();
    });
    assert_eq!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
    assert!(!finalized.load(Ordering::SeqCst));
    release_tx.send(()).unwrap();
    holder.await.unwrap().unwrap();
    publisher.join().unwrap();
}

#[test]
fn claim_crossing_absolute_deadline_releases_without_finalizing_or_revealing() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let enrollment = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    let prepared = run_async(
        enrollment
            .admit_current(deadline, |_| String::from("protected response"))
            .unwrap()
            .prepare_handoff(),
    )
    .unwrap();
    let (claim_paused, resume_claim, claim_won) = prepared.pause_claim_after_precheck();
    let finalized = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let finalized_after = finalized.clone();
    let revealed = Arc::new(AtomicBool::new(false));
    let revealed_after = revealed.clone();
    let (commit_tx, commit_rx) = std::sync::mpsc::channel();
    let commit = thread::spawn(move || {
        let result = run_async(prepared.commit_with(
            |_| Ok(()),
            move |_, ()| {
                finalized_after.fetch_add(1, Ordering::SeqCst);
            },
        ));
        if result.is_ok() {
            revealed_after.store(true, Ordering::SeqCst);
        }
        commit_tx.send(result).unwrap();
    });

    claim_paused
        .recv_timeout(Duration::from_secs(2))
        .expect("commit did not pause between deadline precheck and CAS");
    while Instant::now() < deadline {
        thread::yield_now();
    }

    let (publisher_started_tx, publisher_started_rx) = std::sync::mpsc::sync_channel(0);
    let (published_tx, published_rx) = std::sync::mpsc::channel();
    let publisher = thread::spawn(move || {
        publisher_started_tx.send(()).unwrap();
        published_tx.send(publish(&store, Some(1))).unwrap();
    });
    publisher_started_rx.recv().unwrap();

    resume_claim.send(()).unwrap();
    let error = commit_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("late claim did not return")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::DeadlineExceeded);
    assert!(
        claim_won.load(Ordering::SeqCst),
        "expiry won instead of exercising the post-CAS deadline check"
    );
    assert_eq!(finalized.load(Ordering::SeqCst), 0);
    assert!(!revealed.load(Ordering::SeqCst));
    assert_eq!(
        published_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("late claim retained publication authority"),
        2
    );

    commit.join().unwrap();
    publisher.join().unwrap();
}

#[test]
fn prepared_clones_share_one_commit_slot() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let prepared = run_async(
        e.admit_current(soon(), |_| "response")
            .unwrap()
            .prepare_handoff(),
    )
    .unwrap();
    let clone = prepared.clone();
    assert_eq!(run_async(prepared.commit()).unwrap(), "response");
    assert_eq!(
        run_async(clone.commit()).unwrap_err().code,
        ErrorCode::StoreUnavailable
    );
}

#[test]
fn pending_response_can_be_an_opaque_placeholder_extension() {
    use axum::{
        body::Body,
        response::{IntoResponse, Response},
    };

    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let e = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let admitted: Response = commit_pending(
        e.admit_current(soon(), |_| "admitted body".into_response())
            .unwrap(),
    )
    .unwrap();
    // Repeat admission and exercise the actual PR2 carrier shape: only a harmless placeholder is
    // visible until the outer poll removes and commits the Prepared<Response> extension.
    let pending = e
        .admit_current(soon(), |_| "admitted body".into_response())
        .unwrap();
    let prepared: crate::mcp::enrollment::Prepared<Response> =
        run_async(pending.prepare_handoff()).unwrap();
    let mut placeholder = Response::new(Body::empty());
    placeholder.extensions_mut().insert(prepared);
    let prepared = placeholder
        .extensions_mut()
        .remove::<crate::mcp::enrollment::Prepared<Response>>()
        .unwrap();
    let handed_off = run_async(prepared.commit()).unwrap();

    assert_eq!(admitted.status(), handed_off.status());
}

#[test]
fn fast_latch_orders_after_a_prepared_finalizer_and_rotates_once() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let enrollment = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());
    let generation = enrollment.store_generation();
    let prepared = run_async(
        enrollment
            .admit_current(soon(), |_| "response")
            .unwrap()
            .prepare_handoff(),
    )
    .unwrap();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let finalizer_events = events.clone();
    let (commit_tx, commit_rx) = std::sync::mpsc::channel();
    let commit = thread::spawn(move || {
        let result = run_async(prepared.commit_with(
            |_| Ok(()),
            move |_, ()| {
                finalizer_events.lock().unwrap().push("finalizer-start");
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                finalizer_events.lock().unwrap().push("finalizer-end");
            },
        ));
        commit_tx.send(result).unwrap();
    });
    entered_rx.recv().unwrap();
    replace_with_new_inode(state.path(), "cache.db");
    let observer_enrollment = enrollment.clone();
    let observer_events = events.clone();
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    let observer = thread::spawn(move || {
        let result = observer_enrollment.current_revision(soon());
        observer_events.lock().unwrap().push("latch-return");
        observed_tx.send(result).unwrap();
    });
    while enrollment.is_available() {
        thread::yield_now();
    }
    assert!(
        observed_rx.try_recv().is_err(),
        "latch returned before finalizer"
    );
    release_tx.send(()).unwrap();
    assert_eq!(
        commit_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap(),
        "response"
    );
    assert_eq!(
        observed_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .code,
        ErrorCode::StoreUnavailable
    );
    commit.join().unwrap();
    observer.join().unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        ["finalizer-start", "finalizer-end", "latch-return"]
    );
    let rotated = enrollment.store_generation();
    assert_ne!(rotated, generation);
    assert!(enrollment.current_revision(soon()).is_err());
    assert_eq!(
        enrollment.store_generation(),
        rotated,
        "generation rotated twice"
    );
}

#[test]
fn fast_latch_orders_after_a_lifecycle_finalizer_and_rotates_once() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let enrollment = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());
    let generation = enrollment.store_generation();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let transition_enrollment = enrollment.clone();
    let finalizer_events = events.clone();
    let (transition_tx, transition_rx) = std::sync::mpsc::channel();
    let transition = thread::spawn(move || {
        let result = run_async(transition_enrollment.lifecycle_transition(
            soon(),
            |_| Ok(()),
            move |_, ()| {
                finalizer_events.lock().unwrap().push("finalizer-start");
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                finalizer_events.lock().unwrap().push("finalizer-end");
                "mutated"
            },
        ));
        transition_tx.send(result).unwrap();
    });
    entered_rx.recv().unwrap();
    replace_with_new_inode(state.path(), "cache.db");
    let observer_enrollment = enrollment.clone();
    let observer_events = events.clone();
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    let observer = thread::spawn(move || {
        let result = observer_enrollment.current_revision(soon());
        observer_events.lock().unwrap().push("latch-return");
        observed_tx.send(result).unwrap();
    });
    while enrollment.is_available() {
        thread::yield_now();
    }
    assert!(
        observed_rx.try_recv().is_err(),
        "latch returned before finalizer"
    );
    release_tx.send(()).unwrap();
    assert_eq!(
        transition_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap(),
        "mutated"
    );
    assert_eq!(
        observed_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .code,
        ErrorCode::StoreUnavailable
    );
    transition.join().unwrap();
    observer.join().unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        ["finalizer-start", "finalizer-end", "latch-return"]
    );
    let rotated = enrollment.store_generation();
    assert_ne!(rotated, generation);
    assert!(enrollment.current_revision(soon()).is_err());
    assert_eq!(
        enrollment.store_generation(),
        rotated,
        "generation rotated twice"
    );
}

#[test]
fn queued_canceled_preparation_loses_publication_authority_at_deadline() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let enrollment = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_millis(120);
    let pending = enrollment
        .admit_current(deadline, |_| DropSignal(dropped.clone()))
        .unwrap();
    let boundary = store.publication_lock();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        let (blocker_started_tx, blocker_started_rx) = std::sync::mpsc::channel();
        let (release_blocker_tx, release_blocker_rx) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            blocker_started_tx.send(()).unwrap();
            release_blocker_rx.recv().unwrap();
        });
        blocker_started_rx.recv().unwrap();
        let preparation = tokio::spawn(async move { pending.prepare_handoff().await });
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        preparation.abort();
        tokio::time::sleep(
            deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(40),
        )
        .await;
        assert!(
            !dropped.load(Ordering::SeqCst),
            "expiry dropped arbitrary T"
        );
        drop(
            boundary
                .publish()
                .expect("expiry must release queued authority"),
        );
        release_blocker_tx.send(()).unwrap();
        blocker.await.unwrap();
    });
}

#[test]
fn identity_failure_releases_publication_before_waiting_for_lifecycle() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let enrollment = Arc::new(Enrollment::enroll(state.path(), store.publication_lock()).unwrap());
    let pending = enrollment.admit_current(soon(), |_| "response").unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let transition_enrollment = enrollment.clone();
    let transition = thread::spawn(move || {
        run_async(transition_enrollment.lifecycle_transition(
            soon(),
            |_| Ok(()),
            move |_, ()| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            },
        ))
        .unwrap();
    });
    entered_rx.recv().unwrap();
    replace_with_new_inode(state.path(), "cache.db");
    let (prepared_tx, prepared_rx) = std::sync::mpsc::channel();
    let preparation = thread::spawn(move || {
        prepared_tx
            .send(run_async(pending.prepare_handoff()))
            .unwrap();
    });
    while enrollment.is_available() {
        thread::yield_now();
    }
    assert!(
        prepared_rx.try_recv().is_err(),
        "failure did not wait behind lifecycle"
    );
    drop(
        store
            .publication_lock()
            .publish()
            .expect("decided failure retained publication while ordering latch"),
    );
    release_tx.send(()).unwrap();
    transition.join().unwrap();
    assert_eq!(
        prepared_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .code,
        ErrorCode::StoreUnavailable
    );
    preparation.join().unwrap();
}

struct BlockingDrop {
    started: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

impl Drop for BlockingDrop {
    fn drop(&mut self) {
        self.started.send(()).unwrap();
        self.release.recv().unwrap();
    }
}

#[test]
fn blocking_destructor_expiry_child_helper() {
    if std::env::var_os("BALEYG_BLOCKING_DROP_CHILD").is_none() {
        return;
    }
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let enrollment = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let first_deadline = Instant::now() + Duration::from_millis(80);
    let first = enrollment
        .admit_current(first_deadline, |_| BlockingDrop {
            started: started_tx,
            release: release_rx,
        })
        .unwrap();
    let second_deadline = Instant::now() + Duration::from_millis(260);
    let _second = enrollment
        .admit_current(second_deadline, |_| "later expiry")
        .unwrap();
    thread::sleep(
        first_deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(20),
    );
    let dropping = thread::spawn(move || drop(first));
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let boundary = store.publication_lock();
    let (published_tx, published_rx) = std::sync::mpsc::channel();
    let publisher = thread::spawn(move || {
        drop(boundary.publish().unwrap());
        published_tx.send(()).unwrap();
    });
    published_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("a blocking generic destructor stalled a later expiry job");
    release_tx.send(()).unwrap();
    dropping.join().unwrap();
    publisher.join().unwrap();
}

#[test]
fn blocking_destructor_cannot_stall_the_expiry_service() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("mcp::enrollment_tests::blocking_destructor_expiry_child_helper")
        .arg("--nocapture")
        .env("BALEYG_BLOCKING_DROP_CHILD", "1")
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                status.success(),
                "blocking-destructor child failed: {status}"
            );
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("blocking-destructor child stalled");
        }
        thread::yield_now();
    }
}

#[test]
fn final_handoff_release_does_not_wait_for_boundary_mutex() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let enrollment = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let prepared = run_async(
        enrollment
            .admit_current(soon(), |_| "response")
            .unwrap()
            .prepare_handoff(),
    )
    .unwrap();
    let boundary = store.publication_lock();
    let held = boundary.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = thread::spawn(move || held.hold_state_for_test(entered_tx, release_rx));
    entered_rx.recv().unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let handoff = thread::spawn(move || {
        done_tx.send(run_async(prepared.commit())).unwrap();
    });
    assert_eq!(
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("final handoff blocked on Boundary mutex")
            .unwrap(),
        "response"
    );
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    handoff.join().unwrap();
}

#[test]
fn a_store_boundary_permanently_rejects_a_second_enrollment() {
    let (state, _work, store) = fixture();
    publish(&store, Some(0));
    let enrollment = Enrollment::enroll(state.path(), store.publication_lock()).unwrap();
    let second = match Enrollment::enroll(state.path(), store.publication_lock()) {
        Ok(_) => panic!("a second enrollment was accepted"),
        Err(error) => error,
    };
    assert!(second.to_string().contains("already enrolled"));
    invalidate(&enrollment);
    let bypass = match Enrollment::enroll(state.path(), store.publication_lock()) {
        Ok(_) => panic!("an invalidation bypass enrollment was accepted"),
        Err(error) => error,
    };
    assert!(bypass.to_string().contains("already enrolled"));
    assert_eq!(
        enrollment.admit_current(soon(), |_| ()).unwrap_err().code,
        ErrorCode::StoreUnavailable
    );
}
