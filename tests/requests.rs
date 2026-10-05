//! Queue rows are durable independently of the native index and only the held leader may claim.
use baleyg::{indexer::IndexOptions, store::Store};
use std::fs;

#[test]
fn partially_created_queue_readers_report_busy_without_initializing_schema() {
    use std::os::unix::fs::PermissionsExt;
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("a.js"), "function a() {}\n").unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let queue = store.request_db_path();
    let db = rusqlite::Connection::open(&queue).unwrap();
    drop(db);
    fs::set_permissions(&queue, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::read(&queue).unwrap();
    for result in [
        store.current_request(),
        store.earliest_unfinished_request(),
        store.request_by_id(&uuid::Uuid::new_v4().to_string()),
    ] {
        let error = result.unwrap_err();
        assert!(
            error.to_string().contains("storage_busy"),
            "first creator can have a version-0 file before committed schema: {error:#}"
        );
        assert_eq!(
            fs::read(&queue).unwrap(),
            before,
            "read path must not initialize a partially created queue"
        );
    }
}

#[test]
fn absent_queue_readers_are_existing_only_and_leave_home_bytes_unchanged() {
    for read in ["current", "by_id"] {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("a.js"), "function a() {}\n").unwrap();
        let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
        let queue = store.request_db_path();
        assert!(
            !queue.exists(),
            "fresh Ready index must not eagerly create requests.db"
        );
        let before = std::fs::read_dir(queue.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        let result = match read {
            "current" => store.current_request().unwrap(),
            "by_id" => store
                .request_by_id(&uuid::Uuid::new_v4().to_string())
                .unwrap(),
            _ => unreachable!(),
        };
        assert!(result.is_none());
        assert!(!queue.exists(), "{read} unexpectedly created requests.db");
        let after = std::fs::read_dir(queue.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(before, after, "{read} mutated the Ready queue directory");
    }
}

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
fn published_pin_before_completion_crash_child() {
    let Ok(state) = std::env::var("BALEYG_TEST_CRASH_GAP_STATE") else {
        return;
    };
    let workspace =
        std::path::PathBuf::from(std::env::var("BALEYG_TEST_CRASH_GAP_WORKSPACE").unwrap());
    let proof = std::path::PathBuf::from(std::env::var("BALEYG_TEST_CRASH_GAP_PROOF").unwrap());
    let store = Store::open_for_tests(std::path::Path::new(&state), &workspace).unwrap();
    let options = IndexOptions::new(workspace);
    let owner = store.leader_session().unwrap();
    let claimed = store.claim_request(&owner).unwrap().unwrap();
    assert_eq!(claimed.state, "running");
    let coordinator = baleyg::index_coordinator::IndexJobCoordinator::prepare_with_session(
        &store,
        claimed.expected,
        owner.clone(),
    )
    .unwrap();
    let pin = coordinator
        .run(
            &options,
            &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            |_| {},
        )
        .unwrap();
    assert_eq!(
        store.status().unwrap().revision,
        pin,
        "publication committed"
    );
    assert_eq!(
        store.request_by_id(&claimed.id).unwrap().unwrap().state,
        "running",
        "no terminal write has happened yet"
    );
    let mut marker = fs::File::create(&proof).unwrap();
    use std::io::Write;
    writeln!(
        marker,
        "{} {} {}",
        claimed.id, pin.index_generation, pin.index_revision
    )
    .unwrap();
    marker.sync_all().unwrap();
    // Exit without Rust drops or request completion. Locks vanish as with a crash.
    std::process::exit(37);
}

#[test]
fn real_process_death_after_request_publish_reclaims_without_false_done() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("a.js"), "function a() {}\n").unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let options = IndexOptions::new(workspace.path().to_owned());
    let (before, owner) = baleyg::index_coordinator::reconcile_workspace(
        &store,
        &options,
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        |_| {},
    )
    .unwrap();
    drop(owner);
    let accepted = store.enqueue_request(&options, None).unwrap();
    let proof = state.path().join("post-publication-proof");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("published_pin_before_completion_crash_child")
        .env("BALEYG_TEST_CRASH_GAP_STATE", state.path())
        .env("BALEYG_TEST_CRASH_GAP_WORKSPACE", workspace.path())
        .env("BALEYG_TEST_CRASH_GAP_PROOF", &proof)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(37),
        "child must exit after verified commit; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let proof = fs::read_to_string(&proof).unwrap();
    let fields: Vec<_> = proof.split_whitespace().collect();
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0], accepted.id);
    let parent = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let after_commit = parent.index_baseline().unwrap();
    assert_eq!(after_commit.index_generation.to_string(), fields[1]);
    assert_eq!(after_commit.index_revision.to_string(), fields[2]);
    assert!(after_commit.index_revision > before.index_revision);
    let unfinished = parent.request_by_id(&accepted.id).unwrap().unwrap();
    assert_eq!(
        unfinished.state, "running",
        "the crash cannot pretend the ACK finished"
    );
    assert!(unfinished.revision.is_none());
    let (takeover, new_owner) = baleyg::index_coordinator::reconcile_workspace(
        &parent,
        &options,
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        |_| {},
    )
    .unwrap();
    assert!(takeover.index_revision > after_commit.index_revision);
    assert_eq!(
        baleyg::index_coordinator::drain_requests(&parent, &new_owner).unwrap(),
        1
    );
    let done = parent.request_by_id(&accepted.id).unwrap().unwrap();
    assert_eq!(done.state, "done");
    assert!(
        done.revision.unwrap().index_revision > takeover.index_revision,
        "repeat-after-commit across process death is allowed, false earlier completion is not"
    );
}

#[test]
fn new_leader_reclaims_running_head_before_later_queued_row() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("a.js"), "function a() {}\n").unwrap();
    let original = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let options = IndexOptions::new(workspace.path().to_owned());
    let (_, old_owner) = baleyg::index_coordinator::reconcile_workspace(
        &original,
        &options,
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        |_| {},
    )
    .unwrap();
    let first = original.enqueue_request(&options, None).unwrap();
    let running = original.claim_request(&old_owner).unwrap().unwrap();
    assert_eq!(running.id, first.id);
    assert_eq!(running.state, "running");
    let old_incarnation = running.claim_incarnation.unwrap();
    let next = original.enqueue_request(&options, None).unwrap();
    assert!(
        original.claim_request(&old_owner).unwrap().is_none(),
        "do not skip running FIFO head"
    );
    drop(old_owner);
    drop(original);
    let replacement = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let (_, new_owner) = baleyg::index_coordinator::reconcile_workspace(
        &replacement,
        &options,
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        baleyg::index_coordinator::drain_requests(&replacement, &new_owner).unwrap(),
        2
    );
    let a = replacement.request_by_id(&first.id).unwrap().unwrap();
    let b = replacement.request_by_id(&next.id).unwrap().unwrap();
    assert_eq!((a.state.as_str(), b.state.as_str()), ("done", "done"));
    assert_ne!(a.claim_incarnation.unwrap(), old_incarnation);
    assert!(
        a.revision.unwrap().index_revision < b.revision.unwrap().index_revision,
        "reclaimed running head must publish before later queue admission"
    );
}

#[test]
fn cli_accepted_during_held_exceptional_owner_waits_until_recreation_completes() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("a.js"), "function a() {}\n").unwrap();
    let old = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let options = IndexOptions::new(workspace.path().to_owned());
    let (_, old_owner) = baleyg::index_coordinator::reconcile_workspace(
        &old,
        &options,
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        |_| {},
    )
    .unwrap();
    let roots = baleyg::store::topology::TopologyRoots::isolated_for_tests(
        state.path().join("cache"),
        state.path().join("data"),
    );
    let identity = baleyg::store::topology::WorkspaceIdentity::discover(
        Some(workspace.path()),
        workspace.path(),
    )
    .unwrap();
    fs::write(roots.index_db(&identity), b"bad sqlite index header").unwrap();
    let waiting = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert!(
        waiting.status().is_err(),
        "corrupt index cannot present a ready head"
    );
    let (tx, rx) = std::sync::mpsc::channel();
    let options_for_cli = options.clone();
    let cli = std::thread::spawn(move || {
        let result = baleyg::index_coordinator::enqueue_and_wait(
            &waiting,
            &options_for_cli,
            &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .map(|(pin, _)| pin);
        tx.send(result).unwrap();
    });
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(80))
            .is_err(),
        "an accepted CLI request must not exit recovery_required while old owner holds EX"
    );
    let ack = old.current_request().unwrap().unwrap();
    assert_eq!(
        ack.state, "queued",
        "CLI acknowledgement must already be durable"
    );
    drop(old_owner);
    let pin = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap()
        .unwrap();
    cli.join().unwrap();
    assert_eq!(old.request_by_id(&ack.id).unwrap().unwrap().state, "done");
    assert_eq!(pin.index_revision, 1, "recreated generation first pin");
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

#[test]
fn old_holder_fails_queued_and_running_rows_after_root_is_moved() {
    let state = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("workspace");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.js"), "function a() {}\n").unwrap();
    let store = Store::open_for_tests(state.path(), &root).unwrap();
    let options = IndexOptions::new(root.clone());
    let first = store.enqueue_request(&options, None).unwrap();
    let second = store.enqueue_request(&options, None).unwrap();
    let owner = store.leader_session().unwrap();
    assert_eq!(store.claim_request(&owner).unwrap().unwrap().id, first.id);
    fs::rename(&root, parent.path().join("old-workspace")).unwrap();
    assert_eq!(store.fail_changed_root_requests(&owner).unwrap(), 2);
    assert_eq!(store.fail_changed_root_requests(&owner).unwrap(), 0);
    let db = rusqlite::Connection::open(store.request_db_path()).unwrap();
    for id in [first.id, second.id] {
        let (state, code, finished): (String, String, String) = db
            .query_row(
                "SELECT state,error_code,finished_at FROM requests WHERE id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((state.as_str(), code.as_str()), ("failed", "root_changed"));
        assert!(!finished.is_empty());
    }
    assert!(store.claim_request(&owner).is_err());
}

#[test]
fn replacement_root_can_accept_while_old_holder_is_live_then_recover_index_only() {
    let state = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("workspace");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.js"), "function oldName() {}\n").unwrap();
    let old = Store::open_for_tests(state.path(), &root).unwrap();
    let first = old
        .enqueue_request(&IndexOptions::new(root.clone()), None)
        .unwrap();
    let old_owner = old.leader_session().unwrap();
    fs::rename(&root, parent.path().join("old-workspace")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.js"), "function newName() {}\n").unwrap();
    let replacement = Store::open_for_tests(state.path(), &root).unwrap();
    assert!(replacement.status().is_err());
    assert!(
        replacement.current_request().unwrap().is_none(),
        "healthy replacement root must ignore the old root's newest job row"
    );
    let options = IndexOptions::new(root.clone());
    let second = replacement.enqueue_request(&options, None).unwrap();
    assert_eq!(
        replacement.current_request().unwrap().unwrap().id,
        second.id
    );
    assert_ne!(first.root_inode, second.root_inode);
    assert!(
        replacement.leader_session().is_err(),
        "old holder still fences new work"
    );
    drop(old_owner);
    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover(Some(&root), &root).unwrap();
    let lock = state
        .path()
        .join("cache/indexes")
        .join(format!("{}.lock", identity.root_key));
    let independent_reader =
        baleyg::store::topology::UseGuard::acquire(&lock, false, false).unwrap();
    let index_path = replacement.request_db_path().with_file_name("index.db");
    let index_before = fs::read(&index_path).unwrap();
    let requests_before = fs::read(replacement.request_db_path()).unwrap();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let error =
        baleyg::index_coordinator::reconcile_workspace(&replacement, &options, &cancel, |_| {})
            .unwrap_err();
    assert!(error.to_string().contains("storage_busy"), "{error:#}");
    assert_eq!(fs::read(&index_path).unwrap(), index_before);
    assert_ne!(
        fs::read(replacement.request_db_path()).unwrap(),
        requests_before,
        "root-loss-only failure was durable before EX BUSY"
    );
    let db = rusqlite::Connection::open(replacement.request_db_path()).unwrap();
    let old_code: String = db
        .query_row(
            "SELECT error_code FROM requests WHERE id=?1",
            [&first.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(old_code, "root_changed");
    drop(db);
    drop(independent_reader);
    let (pin, session) =
        baleyg::index_coordinator::enqueue_and_wait(&replacement, &options, &cancel).unwrap();
    assert!(session.is_leader());
    assert_eq!(
        replacement
            .request_by_id(&second.id)
            .unwrap()
            .unwrap()
            .state,
        "done"
    );
    assert_eq!(replacement.status().unwrap().revision, pin);
    let db = rusqlite::Connection::open(replacement.request_db_path()).unwrap();
    let code: String = db
        .query_row(
            "SELECT error_code FROM requests WHERE id=?1",
            [&first.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(code, "root_changed");
}

#[test]
fn unchanged_root_with_changed_workspace_marker_cannot_fail_queue_rows() {
    let state = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join(".git")).unwrap();
    let store = Store::open_for_tests(state.path(), root.path()).unwrap();
    let row = store
        .enqueue_request(&IndexOptions::new(root.path().to_owned()), None)
        .unwrap();
    let owner = store.leader_session().unwrap();
    fs::write(
        root.path().join(".git/baleyg/workspace-id"),
        uuid::Uuid::new_v4().to_string(),
    )
    .unwrap();
    assert!(store.fail_changed_root_requests(&owner).is_err());
    let db = rusqlite::Connection::open(store.request_db_path()).unwrap();
    let state: String = db
        .query_row("SELECT state FROM requests WHERE id=?1", [&row.id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(state, "queued");
}

#[test]
fn replacement_rechecks_old_index_under_ex_before_any_rename() {
    use std::os::unix::fs::MetadataExt;
    let state = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("workspace");
    fs::create_dir(&root).unwrap();
    let old = Store::open_for_tests(state.path(), &root).unwrap();
    fs::rename(&root, parent.path().join("old-workspace")).unwrap();
    fs::create_dir(&root).unwrap();
    let replacement = Store::open_for_tests(state.path(), &root).unwrap();
    let index = replacement.request_db_path().with_file_name("index.db");
    let db = rusqlite::Connection::open(&index).unwrap();
    db.execute(
        "UPDATE index_metadata SET root_inode=?1",
        [fs::metadata(&root).unwrap().ino().to_string()],
    )
    .unwrap();
    drop(db);
    let before = fs::read(&index).unwrap();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let error = baleyg::index_coordinator::reconcile_workspace(
        &replacement,
        &IndexOptions::new(root),
        &cancel,
        |_| {},
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("replacement authority changed"),
        "{error:#}"
    );
    assert_eq!(fs::read(&index).unwrap(), before);
    drop(old);
}

#[test]
fn replacement_queue_write_refusal_preserves_old_index_before_ex() {
    let state = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("workspace");
    fs::create_dir(&root).unwrap();
    let old = Store::open_for_tests(state.path(), &root).unwrap();
    let old_request = old
        .enqueue_request(&IndexOptions::new(root.clone()), None)
        .unwrap();
    fs::rename(&root, parent.path().join("old-workspace")).unwrap();
    fs::create_dir(&root).unwrap();
    let replacement = Store::open_for_tests(state.path(), &root).unwrap();
    let index = replacement.request_db_path().with_file_name("index.db");
    let db = rusqlite::Connection::open(replacement.request_db_path()).unwrap();
    db.execute_batch("CREATE TRIGGER refuse_root_failure BEFORE UPDATE ON requests WHEN NEW.error_code='root_changed' BEGIN SELECT RAISE(ABORT,'injected queue write failure'); END;").unwrap();
    drop(db);
    let index_before = fs::read(&index).unwrap();
    let queue_before = fs::read(replacement.request_db_path()).unwrap();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    assert!(
        baleyg::index_coordinator::reconcile_workspace(
            &replacement,
            &IndexOptions::new(root),
            &cancel,
            |_| {}
        )
        .is_err()
    );
    assert_eq!(fs::read(&index).unwrap(), index_before);
    assert_eq!(
        fs::read(replacement.request_db_path()).unwrap(),
        queue_before
    );
    let db = rusqlite::Connection::open(replacement.request_db_path()).unwrap();
    let state: String = db
        .query_row(
            "SELECT state FROM requests WHERE id=?1",
            [&old_request.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "queued");
}

#[test]
fn replacement_refuses_foreign_index_marker_and_sidecars_without_queue_transition() {
    for foreign in ["marker", "wal", "journal"] {
        let state = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let old = Store::open_for_tests(state.path(), &root).unwrap();
        old.enqueue_request(&IndexOptions::new(root.clone()), None)
            .unwrap();
        fs::rename(&root, parent.path().join("old-workspace")).unwrap();
        fs::create_dir(&root).unwrap();
        let replacement = Store::open_for_tests(state.path(), &root).unwrap();
        let index = replacement.request_db_path().with_file_name("index.db");
        match foreign {
            "marker" => {
                let db = rusqlite::Connection::open(&index).unwrap();
                db.execute("UPDATE index_metadata SET root_spelling='foreign'", [])
                    .unwrap();
            }
            "wal" => fs::write(index.with_file_name("index.db-wal"), b"foreign").unwrap(),
            _ => fs::write(index.with_file_name("index.db-journal"), b"foreign").unwrap(),
        }
        let index_before = fs::read(&index).unwrap();
        let queue_before = fs::read(replacement.request_db_path()).unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert!(
            baleyg::index_coordinator::reconcile_workspace(
                &replacement,
                &IndexOptions::new(root),
                &cancel,
                |_| {}
            )
            .is_err(),
            "{foreign}"
        );
        assert_eq!(fs::read(&index).unwrap(), index_before, "{foreign}");
        assert_eq!(
            fs::read(replacement.request_db_path()).unwrap(),
            queue_before,
            "{foreign}"
        );
        drop(old);
    }
}

#[test]
fn replacement_refuses_missing_or_foreign_queue_without_swapping_index() {
    for fault in ["missing", "foreign", "incompatible", "empty"] {
        let state = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let old = Store::open_for_tests(state.path(), &root).unwrap();
        old.enqueue_request(&IndexOptions::new(root.clone()), None)
            .unwrap();
        fs::rename(&root, parent.path().join("old-workspace")).unwrap();
        fs::create_dir(&root).unwrap();
        let replacement = Store::open_for_tests(state.path(), &root).unwrap();
        let queue = replacement.request_db_path();
        let index = queue.with_file_name("index.db");
        match fault {
            "missing" => fs::remove_file(&queue).unwrap(),
            "empty" => {
                fs::remove_file(&queue).unwrap();
                fs::write(&queue, b"").unwrap();
            }
            _ => {
                let db = rusqlite::Connection::open(&queue).unwrap();
                if fault == "foreign" {
                    db.execute("UPDATE queue_identity SET root_key='foreign'", [])
                        .unwrap();
                } else {
                    db.pragma_update(None, "user_version", 2).unwrap();
                }
            }
        }
        let before_index = fs::read(&index).unwrap();
        let before_queue = if queue.exists() {
            Some(fs::read(&queue).unwrap())
        } else {
            None
        };
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert!(
            baleyg::index_coordinator::reconcile_workspace(
                &replacement,
                &IndexOptions::new(root),
                &cancel,
                |_| {}
            )
            .is_err(),
            "{fault}"
        );
        assert_eq!(fs::read(&index).unwrap(), before_index, "{fault}");
        assert_eq!(queue.exists(), before_queue.is_some(), "{fault}");
        if let Some(bytes) = before_queue {
            assert_eq!(fs::read(&queue).unwrap(), bytes, "{fault}");
        }
        drop(old);
    }
}

#[test]
fn root_failure_commit_error_never_authorizes_index_swap() {
    let state = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("workspace");
    fs::create_dir(&root).unwrap();
    let old = Store::open_for_tests(state.path(), &root).unwrap();
    let row = old
        .enqueue_request(&IndexOptions::new(root.clone()), None)
        .unwrap();
    fs::rename(&root, parent.path().join("old-workspace")).unwrap();
    fs::create_dir(&root).unwrap();
    let replacement = Store::open_for_tests(state.path(), &root).unwrap();
    let queue = replacement.request_db_path();
    let index = queue.with_file_name("index.db");
    let db = rusqlite::Connection::open(&queue).unwrap();
    db.execute_batch("CREATE TABLE commit_parent(id INTEGER PRIMARY KEY); CREATE TABLE commit_failure(id INTEGER REFERENCES commit_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER fail_commit AFTER UPDATE ON requests WHEN NEW.error_code='root_changed' BEGIN INSERT INTO commit_failure(id) VALUES (1); END;").unwrap();
    drop(db);
    let before_index = fs::read(&index).unwrap();
    let before_queue = fs::read(&queue).unwrap();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let error = baleyg::index_coordinator::reconcile_workspace(
        &replacement,
        &IndexOptions::new(root),
        &cancel,
        |_| {},
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("FOREIGN KEY"), "{error:#}");
    assert_eq!(fs::read(&index).unwrap(), before_index);
    assert_eq!(fs::read(&queue).unwrap(), before_queue);
    let db = rusqlite::Connection::open(&queue).unwrap();
    let state: String = db
        .query_row("SELECT state FROM requests WHERE id=?1", [&row.id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(state, "queued");
}

#[test]
fn live_old_holder_rejects_replaced_same_key_queue_inode() {
    let state = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("workspace");
    fs::create_dir(&root).unwrap();
    let store = Store::open_for_tests(state.path(), &root).unwrap();
    store
        .enqueue_request(&IndexOptions::new(root.clone()), None)
        .unwrap();
    let owner = store.leader_session().unwrap();
    let queue = store.request_db_path();
    let copy = queue.with_file_name("requests.db.tmp-copy");
    fs::copy(&queue, &copy).unwrap();
    fs::rename(&copy, &queue).unwrap();
    fs::rename(&root, parent.path().join("old-workspace")).unwrap();
    let index = queue.with_file_name("index.db");
    let before = fs::read(&index).unwrap();
    let error = store.fail_changed_root_requests(&owner).unwrap_err();
    assert!(error.to_string().contains("inode replaced"), "{error:#}");
    assert_eq!(fs::read(&index).unwrap(), before);
}

#[test]
fn replacement_follower_never_recreates_deleted_accepted_queue_on_enqueue() {
    let state = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("workspace");
    fs::create_dir(&root).unwrap();
    let old = Store::open_for_tests(state.path(), &root).unwrap();
    old.enqueue_request(&IndexOptions::new(root.clone()), None)
        .unwrap();
    let owner = old.leader_session().unwrap();
    fs::rename(&root, parent.path().join("old-workspace")).unwrap();
    fs::create_dir(&root).unwrap();
    let replacement = Store::open_for_tests(state.path(), &root).unwrap();
    let queue = replacement.request_db_path();
    let index = queue.with_file_name("index.db");
    let index_before = fs::read(&index).unwrap();
    fs::remove_file(&queue).unwrap();
    assert!(
        replacement
            .enqueue_request(&IndexOptions::new(root.clone()), None)
            .is_err()
    );
    assert!(
        !queue.exists(),
        "replacement ACK must not create an empty queue"
    );
    assert_eq!(fs::read(&index).unwrap(), index_before);
    drop(owner);
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    assert!(
        baleyg::index_coordinator::reconcile_workspace(
            &replacement,
            &IndexOptions::new(root),
            &cancel,
            |_| {}
        )
        .is_err()
    );
    assert!(!queue.exists());
    assert_eq!(fs::read(&index).unwrap(), index_before);
}
